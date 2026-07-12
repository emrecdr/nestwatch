//! Controller used when running as the SYSTEM service (Session 0).
//!
//! Process listing/kill and shutdown work fine from Session 0, so those delegate straight
//! to [`WindowsControl`]. Screen capture does NOT work from Session 0 (no desktop), so it
//! is delegated to a helper launched into the interactive user session (see `crate::session`).

use super::windows::WindowsControl;
use super::{ControlError, ProcessInfo, SessionState, SystemControl};

pub struct ServiceControl {
    inner: WindowsControl,
}

impl ServiceControl {
    pub fn new() -> Self {
        Self {
            inner: WindowsControl::new(),
        }
    }
}

impl SystemControl for ServiceControl {
    fn screenshot_png(&self) -> Result<Vec<u8>, ControlError> {
        crate::session::capture_via_session_helper()
    }

    fn list_processes(&self) -> Result<Vec<ProcessInfo>, ControlError> {
        self.inner.list_processes()
    }

    fn kill_process(&self, pid: u32) -> Result<(), ControlError> {
        self.inner.kill_process(pid)
    }

    fn lock_workstation(&self) -> Result<(), ControlError> {
        // A Session-0 process can't lock the interactive desktop directly (same reason
        // screenshots need the helper), so launch the lock inside the user's session.
        crate::session::lock_active_session()
    }

    fn shutdown(&self, delay_secs: u32, message: Option<String>) -> Result<(), ControlError> {
        self.inner.shutdown(delay_secs, message)
    }

    fn abort_shutdown(&self) -> Result<(), ControlError> {
        self.inner.abort_shutdown()
    }

    fn session_state(&self) -> Result<SessionState, ControlError> {
        // Session 0 has no session of its own to inspect; query the active console session
        // (the child's) directly via WTS. No helper needed — WTS works from the service.
        self.inner.session_state()
    }
}
