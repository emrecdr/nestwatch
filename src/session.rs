//! Windows-only: launch a short-lived helper process **in the interactive user session**
//! from the SYSTEM service, so it can capture the screen (which Session 0 cannot).
//!
//! The helper's PNG is streamed back over an **inherited stdout pipe** — no temp file — so
//! there's nothing on disk for a standard user to read, spoof, or squat, and no torn-read
//! race. A watchdog thread terminates the helper if it exceeds the timeout.
//!
//! Flow: find the active console session → get its user token → duplicate to a primary
//! token → create a pipe (child-inheritable write end) → `CreateProcessAsUserW` running
//! `<exe> helper --capture-stdout` on the user's desktop with stdout = pipe → read the pipe
//! to EOF → PNG bytes.
//!
//! Requires `SE_TCB_NAME` (SYSTEM has it). All `unsafe` FFI; compile/link-checked via the
//! Windows target and must be runtime-verified on an actual Windows machine.

use std::io::Read;
use std::os::windows::io::FromRawHandle;
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation, WAIT_TIMEOUT,
};
use windows::Win32::Security::{
    DuplicateTokenEx, SECURITY_ATTRIBUTES, SecurityImpersonation, TOKEN_ALL_ACCESS, TokenPrimary,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_SESSIONSTATE_LOCK, WTSFreeMemory, WTSGetActiveConsoleSessionId,
    WTSINFOEXW, WTSQuerySessionInformationW, WTSQueryUserToken, WTSSendMessageW, WTSSessionInfoEx,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONWARNING, MB_OK, MB_SYSTEMMODAL, MESSAGEBOX_RESULT,
};
use windows::core::{PCWSTR, PWSTR};

use crate::control::{ControlError, SessionState};

const HELPER_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a notification box stays before auto-dismissing (seconds). Long enough to read a
/// "locking soon" warning, short enough not to linger.
const NOTIFY_TIMEOUT_SECS: u32 = 30;

/// Capture the screen by delegating to a helper in the interactive session, reading its
/// PNG output over a pipe.
pub fn capture_via_session_helper() -> Result<Vec<u8>, ControlError> {
    let exe = std::env::current_exe().map_err(|e| ControlError::Capture(e.to_string()))?;
    let png = spawn_and_capture(&exe.to_string_lossy())?;
    if png.is_empty() {
        return Err(ControlError::Capture(
            "helper produced no screenshot; is a user logged in?".into(),
        ));
    }
    Ok(png)
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn cap(context: &str, e: impl std::fmt::Display) -> ControlError {
    ControlError::Capture(format!("{context}: {e}"))
}

/// Acquire a **primary token** for the user in the active console session, so a Session-0
/// process can `CreateProcessAsUserW` into their desktop. The caller owns the returned handle
/// and must `CloseHandle` it. Errors are returned as plain strings so each caller can wrap
/// them in the right [`ControlError`] variant.
///
/// # Safety
///
/// Callers own the returned handle and must `CloseHandle` it; leaking it holds a primary token
/// for the child's session open for the life of the service.
unsafe fn active_session_token() -> Result<HANDLE, String> {
    // SAFETY: Win32 token FFI; the intermediate `user_token` is always closed before returning.
    unsafe {
        let session_id = WTSGetActiveConsoleSessionId();
        if session_id == u32::MAX {
            return Err("no active console session".into());
        }
        let mut user_token = HANDLE::default();
        WTSQueryUserToken(session_id, &mut user_token)
            .map_err(|e| format!("WTSQueryUserToken: {e}"))?;
        let mut primary = HANDLE::default();
        let dup = DuplicateTokenEx(
            user_token,
            TOKEN_ALL_ACCESS,
            None::<*const SECURITY_ATTRIBUTES>,
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        );
        let _ = CloseHandle(user_token);
        dup.map_err(|e| format!("DuplicateTokenEx: {e}"))?;
        Ok(primary)
    }
}

/// Determine whether an interactive user is present at the active console session, and whether
/// they're actively using it (unlocked) or the screen is locked. Queried directly via WTS, so
/// it works from the SYSTEM service (Session 0) without a user-session helper.
///
/// Maps to [`SessionState`]: no console session or no logged-on user → `NoUser`; a logged-on
/// user with the workstation locked → `Locked`; otherwise → `Active`. On any query failure the
/// error is returned so the caller can fail toward enforcement.
pub fn active_session_state() -> Result<SessionState, ControlError> {
    // SAFETY: Win32 WTS FFI. The buffer returned by `WTSQuerySessionInformationW` is freed with
    // `WTSFreeMemory` on every path before returning.
    unsafe {
        let session_id = WTSGetActiveConsoleSessionId();
        if session_id == u32::MAX {
            // No session attached to the physical console (e.g. the machine is booting or off).
            return Ok(SessionState::NoUser);
        }

        let mut buffer = PWSTR::null();
        let mut bytes: u32 = 0;
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            session_id,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes,
        )
        .map_err(|e| ControlError::Op(format!("WTSQuerySessionInformationW: {e}")))?;

        if buffer.is_null() || (bytes as usize) < std::mem::size_of::<WTSINFOEXW>() {
            if !buffer.is_null() {
                WTSFreeMemory(buffer.0 as *mut core::ffi::c_void);
            }
            return Err(ControlError::Op("WTSSessionInfoEx returned no data".into()));
        }

        // The buffer is a `WTSINFOEXW`; interpret the Level-1 payload.
        let info = &*(buffer.0 as *const WTSINFOEXW);
        let state = if info.Level != 1 {
            // Unexpected payload level — can't interpret, so assume in use (fail toward
            // enforcement rather than handing out unlimited time).
            SessionState::Active
        } else {
            let level1 = &info.Data.WTSInfoExLevel1;
            if level1.UserName[0] == 0 {
                // No logged-on user: the session exists but is at the sign-in screen.
                SessionState::NoUser
            } else if level1.SessionFlags == WTS_SESSIONSTATE_LOCK as i32 {
                SessionState::Locked
            } else {
                // WTS_SESSIONSTATE_UNLOCK (or UNKNOWN) → treat as actively in use.
                SessionState::Active
            }
        };

        WTSFreeMemory(buffer.0 as *mut core::ffi::c_void);
        Ok(state)
    }
}

/// Show a brief, non-blocking notification on the active console session's desktop via
/// `WTSSendMessageW`. Works from the SYSTEM service (Session 0): the message is targeted at the
/// child's session by id, so it appears on *their* desktop — no user-session helper needed.
/// `bWait = false` returns immediately (the box auto-dismisses after [`NOTIFY_TIMEOUT_SECS`]),
/// so the enforcer never blocks waiting for a click.
pub fn notify_active_session(title: &str, body: &str) -> Result<(), ControlError> {
    // SAFETY: Win32 WTS FFI. The wide buffers are borrowed for the duration of this synchronous
    // call only; no handle is retained.
    unsafe {
        let session_id = WTSGetActiveConsoleSessionId();
        if session_id == u32::MAX {
            return Err(ControlError::Op("no active console session".into()));
        }
        let title_w = to_wide(title);
        let body_w = to_wide(body);
        let mut response = MESSAGEBOX_RESULT(0);
        // Lengths are in BYTES and exclude the trailing NUL (`to_wide` appends one).
        WTSSendMessageW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            session_id,
            PCWSTR(title_w.as_ptr()),
            ((title_w.len() - 1) * 2) as u32,
            PCWSTR(body_w.as_ptr()),
            ((body_w.len() - 1) * 2) as u32,
            MB_OK | MB_ICONWARNING | MB_SYSTEMMODAL,
            NOTIFY_TIMEOUT_SECS,
            &mut response,
            false, // don't wait for the user to dismiss
        )
        .map_err(|e| ControlError::Op(format!("WTSSendMessageW: {e}")))
    }
}

/// Lock the interactive session from the SYSTEM service by launching `<exe> helper --lock`
/// inside the active console session (a Session-0 process can't lock the user's desktop
/// directly). Fire-and-forget: the helper runs `LockWorkStation` and exits.
pub fn lock_active_session() -> Result<(), ControlError> {
    let exe = std::env::current_exe().map_err(|e| ControlError::Op(e.to_string()))?;
    spawn_lock(&exe.to_string_lossy())
}

fn spawn_lock(exe: &str) -> Result<(), ControlError> {
    // SAFETY: Win32 token/process FFI; every handle acquired is released on all paths.
    unsafe {
        let primary = active_session_token().map_err(ControlError::Op)?;

        let mut env_block: *mut core::ffi::c_void = std::ptr::null_mut();
        let have_env = CreateEnvironmentBlock(&mut env_block, Some(primary), false).is_ok();

        let mut desktop = to_wide(r"winsta0\default");
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };

        let mut cmdline = to_wide(&format!("\"{exe}\" helper --lock"));
        let mut proc_info = PROCESS_INFORMATION::default();
        let spawn = CreateProcessAsUserW(
            Some(primary),
            None,
            Some(PWSTR(cmdline.as_mut_ptr())),
            None::<*const SECURITY_ATTRIBUTES>,
            None::<*const SECURITY_ATTRIBUTES>,
            false, // no inherited handles (no pipe)
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            if have_env { Some(env_block) } else { None },
            None,
            &startup,
            &mut proc_info,
        );

        if have_env {
            let _ = DestroyEnvironmentBlock(env_block);
        }
        let _ = CloseHandle(primary);

        spawn.map_err(|e| ControlError::Op(format!("CreateProcessAsUserW: {e}")))?;
        // The helper is short-lived; we don't wait on it. Closing our handles doesn't stop it.
        let _ = CloseHandle(proc_info.hProcess);
        let _ = CloseHandle(proc_info.hThread);
        Ok(())
    }
}

/// Keep a foreground watcher alive in the interactive session, feeding everything it reports into
/// `feed`. Runs forever on its own thread; the caller spawns it once at startup.
///
/// **Respawns, with backoff.** The watcher runs as the child, in a session the child controls, so
/// it can be killed from Task Manager at any moment — and it legitimately dies at every sign-out.
/// Neither may end screen-time measurement for good. The backoff is what stops a watcher that
/// cannot start (no user signed in, a broken build) from becoming a spawn loop.
///
/// Note what is deliberately *not* done here: a failure is never fatal and never blocks. If no
/// interactive user exists, this sleeps and tries again — that is the normal state of a PC sitting
/// at the sign-in screen.
pub fn run_watcher_supervisor(feed: crate::foreground::Feed) {
    /// Backoff bounds. The floor keeps a legitimate sign-out/sign-in cheap; the ceiling keeps a
    /// permanently-failing spawn down to twice a minute.
    const MIN_BACKOFF: Duration = Duration::from_secs(5);
    const MAX_BACKOFF: Duration = Duration::from_secs(30);

    let mut backoff = MIN_BACKOFF;
    loop {
        match std::env::current_exe() {
            Ok(exe) => match pump_watcher(&exe.to_string_lossy(), &feed) {
                // A clean exit means the session ended: retry promptly, the child may be signing
                // straight back in.
                Ok(()) => backoff = MIN_BACKOFF,
                Err(e) => {
                    tracing::debug!(error = %e, "foreground watcher stopped");
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "cannot locate own exe to start the watcher");
                backoff = MAX_BACKOFF;
            }
        }
        std::thread::sleep(backoff);
    }
}

/// Spawn one watcher and read its output until the pipe closes. Returns when it exits.
fn pump_watcher(exe: &str, feed: &crate::foreground::Feed) -> Result<(), ControlError> {
    use std::io::BufRead;

    let (reader, proc_info) = spawn_piped(exe, "helper --watch")?;
    let mut lines = std::io::BufReader::new(reader).lines();

    while let Some(Ok(line)) = lines.next() {
        // A malformed line is skipped, never fatal: this pipe can be cut mid-write by a session
        // ending, so a partial line is expected rather than exceptional.
        if let Some(sample) = crate::foreground::parse_sample(&line) {
            feed.submit(sample);
        }
    }

    // SAFETY: both handles were produced by `CreateProcessAsUserW` in `spawn_piped` and are closed
    // exactly once, here, after the pipe has reached EOF.
    unsafe {
        let _ = CloseHandle(proc_info.hProcess);
        let _ = CloseHandle(proc_info.hThread);
    }
    Ok(())
}

/// Launch `<exe> <args>` in the active console session with stdout wired to a pipe, and hand back
/// the read end plus the process handles.
///
/// Shared by the screenshot helper (which reads one PNG and lets a watchdog bound it) and the
/// foreground watcher (which streams JSON lines for the life of the session). The difference
/// between those two is entirely in what the caller does with the pipe — so the token, pipe,
/// environment block and desktop handling live here once rather than being copied and drifting.
///
/// The caller owns both returned handles and must `CloseHandle` them; the `File` closes the read
/// end on drop.
fn spawn_piped(
    exe: &str,
    args: &str,
) -> Result<(std::fs::File, PROCESS_INFORMATION), ControlError> {
    // SAFETY: Win32 token/pipe/process FFI. Every handle acquired here is either released before
    // returning or handed to the caller, and the read end becomes a File that closes on drop.
    unsafe {
        let primary = active_session_token().map_err(ControlError::Capture)?;

        // Pipe: child inherits the write end; parent keeps the (non-inheritable) read end.
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        let mut read = HANDLE::default();
        let mut write = HANDLE::default();
        if let Err(e) = CreatePipe(&mut read, &mut write, Some(&sa), 0) {
            let _ = CloseHandle(primary);
            return Err(cap("CreatePipe", e));
        }
        let _ = SetHandleInformation(read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));

        // Environment block for the target user (so %PATH% etc. resolve on their side).
        let mut env_block: *mut core::ffi::c_void = std::ptr::null_mut();
        let have_env = CreateEnvironmentBlock(&mut env_block, Some(primary), false).is_ok();

        let mut desktop = to_wide(r"winsta0\default");
        // hStdError/hStdInput are left null by `..Default::default()`: the helper writes only its
        // payload to stdout, so nothing can corrupt the stream.
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            dwFlags: STARTF_USESTDHANDLES,
            hStdOutput: write,
            ..Default::default()
        };

        let mut cmdline = to_wide(&format!("\"{exe}\" {args}"));
        let mut proc_info = PROCESS_INFORMATION::default();
        let spawn = CreateProcessAsUserW(
            Some(primary),
            None,
            Some(PWSTR(cmdline.as_mut_ptr())),
            None::<*const SECURITY_ATTRIBUTES>,
            None::<*const SECURITY_ATTRIBUTES>,
            true, // inherit handles (the pipe write end)
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            if have_env { Some(env_block) } else { None },
            None,
            &startup,
            &mut proc_info,
        );

        // Parent no longer needs the write end (must close it to receive EOF), the env
        // block, or the token.
        let _ = CloseHandle(write);
        if have_env {
            let _ = DestroyEnvironmentBlock(env_block);
        }
        let _ = CloseHandle(primary);

        if let Err(e) = spawn {
            let _ = CloseHandle(read);
            return Err(cap("CreateProcessAsUserW", e));
        }

        Ok((std::fs::File::from_raw_handle(read.0), proc_info))
    }
}

/// Launch `<exe> helper --capture-stdout` in the active console session and return the PNG bytes
/// it writes, bounded by a watchdog so a wedged helper cannot block the caller forever.
fn spawn_and_capture(exe: &str) -> Result<Vec<u8>, ControlError> {
    let (mut file, proc_info) = spawn_piped(exe, "helper --capture-stdout")?;

    // SAFETY: Win32 process FFI. `proc_info`'s handles come from `spawn_piped` and are closed
    // exactly once, below, after the watchdog has finished using them.
    unsafe {
        // Watchdog: kill the helper if it outruns the timeout (unblocks the read via EOF).
        let proc_raw = proc_info.hProcess.0 as isize;
        let watchdog = std::thread::spawn(move || {
            let handle = HANDLE(proc_raw as *mut core::ffi::c_void);
            if WaitForSingleObject(handle, HELPER_TIMEOUT.as_millis() as u32) == WAIT_TIMEOUT {
                let _ = TerminateProcess(handle, 1);
            }
        });

        // Read the PNG from the pipe (File owns the read handle and closes it on drop).
        let mut buf = Vec::new();
        let read_result = file.read_to_end(&mut buf);
        drop(file);

        let _ = watchdog.join(); // done using hProcess before we close it below
        let _ = CloseHandle(proc_info.hProcess);
        let _ = CloseHandle(proc_info.hThread);

        read_result.map_err(|e| cap("read pipe", e))?;
        Ok(buf)
    }
}
