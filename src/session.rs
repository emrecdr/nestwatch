//! Windows-only: launch a short-lived helper process **in the interactive user session**
//! from the SYSTEM service, so it can capture the screen (which Session 0 cannot).
//!
//! Flow: find the active console session → get its user token → duplicate it to a primary
//! token → `CreateProcessAsUserW` running `<exe> helper --capture <file>` on the user's
//! desktop → wait → read the PNG the helper wrote → clean up.
//!
//! Requires `SE_TCB_NAME` (SYSTEM has it), which is why this only works from the service.
//! Everything here is `unsafe` FFI; it is compile-checked via the Windows target and must
//! be runtime-verified on an actual Windows machine.

use std::path::PathBuf;
use std::time::Duration;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, TokenPrimary, SECURITY_ATTRIBUTES, TOKEN_ALL_ACCESS,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT,
    PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::control::ControlError;

const HELPER_TIMEOUT: Duration = Duration::from_secs(15);

/// Capture the screen by delegating to a helper in the interactive session.
pub fn capture_via_session_helper() -> Result<Vec<u8>, ControlError> {
    // A location writable by the standard user and readable by SYSTEM.
    let public = std::env::var("PUBLIC").unwrap_or_else(|_| r"C:\Users\Public".to_string());
    let out = PathBuf::from(public).join(format!("hh-cap-{}.png", std::process::id()));

    let exe = std::env::current_exe().map_err(|e| ControlError::Capture(e.to_string()))?;
    let out_str = out.to_string_lossy().to_string();

    // Best-effort clean any stale file first.
    let _ = std::fs::remove_file(&out);

    spawn_in_active_session(&exe.to_string_lossy(), &out_str)?;

    let bytes = std::fs::read(&out).map_err(|e| {
        ControlError::Capture(format!("helper produced no screenshot ({e}); is a user logged in?"))
    })?;
    let _ = std::fs::remove_file(&out);
    Ok(bytes)
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Launch `"<exe>" helper --capture "<out_path>"` in the active console session.
fn spawn_in_active_session(exe: &str, out_path: &str) -> Result<(), ControlError> {
    unsafe {
        let session_id = WTSGetActiveConsoleSessionId();
        if session_id == u32::MAX {
            return Err(ControlError::Capture("no active console session".into()));
        }

        // Token of the user logged into the console session.
        let mut user_token = HANDLE::default();
        WTSQueryUserToken(session_id, &mut user_token)
            .map_err(|e| ControlError::Capture(format!("WTSQueryUserToken: {e}")))?;

        // Duplicate to a primary token suitable for CreateProcessAsUser.
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
        dup.map_err(|e| ControlError::Capture(format!("DuplicateTokenEx: {e}")))?;

        // Environment block for the target user (so paths like %PUBLIC% resolve correctly).
        let mut env_block: *mut core::ffi::c_void = std::ptr::null_mut();
        let have_env = CreateEnvironmentBlock(&mut env_block, Some(primary), false).is_ok();

        let mut startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut desktop = to_wide(r"winsta0\default");
        startup.lpDesktop = PWSTR(desktop.as_mut_ptr());

        let mut cmdline = to_wide(&format!("\"{exe}\" helper --capture \"{out_path}\""));
        let mut proc_info = PROCESS_INFORMATION::default();

        let result = CreateProcessAsUserW(
            Some(primary),
            None,
            Some(PWSTR(cmdline.as_mut_ptr())),
            None::<*const SECURITY_ATTRIBUTES>,
            None::<*const SECURITY_ATTRIBUTES>,
            false,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            if have_env { Some(env_block) } else { None },
            None,
            &startup,
            &mut proc_info,
        );

        // Clean up regardless of outcome.
        if have_env {
            let _ = DestroyEnvironmentBlock(env_block);
        }
        let _ = CloseHandle(primary);

        result.map_err(|e| ControlError::Capture(format!("CreateProcessAsUserW: {e}")))?;

        // Wait for the helper to finish writing the file, then release its handles.
        WaitForSingleObject(proc_info.hProcess, HELPER_TIMEOUT.as_millis() as u32);
        let _ = CloseHandle(proc_info.hProcess);
        let _ = CloseHandle(proc_info.hThread);
    }
    Ok(())
}
