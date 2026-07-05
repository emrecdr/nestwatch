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
use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows::core::PWSTR;

use crate::control::ControlError;

const HELPER_TIMEOUT: Duration = Duration::from_secs(15);

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

/// Launch `<exe> helper --capture-stdout` in the active console session with stdout wired to
/// a pipe, and return the PNG bytes it writes.
fn spawn_and_capture(exe: &str) -> Result<Vec<u8>, ControlError> {
    // SAFETY: Win32 token/pipe/process FFI. Every handle acquired is released on all paths
    // (the read end is handed to a File which closes it on drop).
    unsafe {
        let session_id = WTSGetActiveConsoleSessionId();
        if session_id == u32::MAX {
            return Err(ControlError::Capture("no active console session".into()));
        }

        // Token of the user in the console session, duplicated to a primary token.
        let mut user_token = HANDLE::default();
        WTSQueryUserToken(session_id, &mut user_token).map_err(|e| cap("WTSQueryUserToken", e))?;
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
        dup.map_err(|e| cap("DuplicateTokenEx", e))?;

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
        // hStdError/hStdInput are left null by `..Default::default()`: the helper writes only
        // the PNG to stdout, so nothing can corrupt the byte stream.
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            dwFlags: STARTF_USESTDHANDLES,
            hStdOutput: write,
            ..Default::default()
        };

        let mut cmdline = to_wide(&format!("\"{exe}\" helper --capture-stdout"));
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

        // Watchdog: kill the helper if it outruns the timeout (unblocks the read via EOF).
        let proc_raw = proc_info.hProcess.0 as isize;
        let watchdog = std::thread::spawn(move || {
            let handle = HANDLE(proc_raw as *mut core::ffi::c_void);
            if WaitForSingleObject(handle, HELPER_TIMEOUT.as_millis() as u32) == WAIT_TIMEOUT {
                let _ = TerminateProcess(handle, 1);
            }
        });

        // Read the PNG from the pipe (File owns the read handle and closes it on drop).
        let mut file = std::fs::File::from_raw_handle(read.0);
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
