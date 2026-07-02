//! The OS abstraction boundary.
//!
//! Everything the app can *do* to the machine goes through [`SystemControl`]. The web
//! layer depends only on this trait, never on `xcap`/`sysinfo`/`shutdown` directly, so:
//!   * the real Windows behaviour is quarantined in `windows.rs`,
//!   * a deterministic [`FakeControl`] lets the whole server build and be tested on macOS,
//!   * new capabilities (e.g. live streaming) can be added without touching handlers.
//!
//! Methods are **synchronous** on purpose: they wrap blocking OS calls. Handlers invoke
//! them via `tokio::task::spawn_blocking` so the async runtime is never stalled, and the
//! trait stays `dyn`-compatible without needing `async-trait`.

use std::sync::Arc;

use serde::Serialize;

mod fake;
#[cfg(windows)]
mod service_control;
#[cfg(windows)]
mod windows;

pub use fake::FakeControl;

/// A single running process, as surfaced to the dashboard.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    /// Resident memory in bytes (rendered human-readably in the UI).
    pub memory_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("no process with pid {0}")]
    ProcessNotFound(u32),

    #[error("screen capture failed: {0}")]
    Capture(String),

    #[error("operation failed: {0}")]
    Op(String),
}

/// The set of remote operations the server can perform on the host machine.
pub trait SystemControl: Send + Sync + 'static {
    /// Capture the primary monitor and return PNG-encoded bytes.
    fn screenshot_png(&self) -> Result<Vec<u8>, ControlError>;

    /// List currently running processes.
    fn list_processes(&self) -> Result<Vec<ProcessInfo>, ControlError>;

    /// Terminate the process with the given PID.
    fn kill_process(&self, pid: u32) -> Result<(), ControlError>;

    /// Begin an orderly shutdown of the machine after `delay_secs`, optionally showing the
    /// user a warning `message` during the countdown.
    fn shutdown(&self, delay_secs: u32, message: Option<String>) -> Result<(), ControlError>;

    /// Cancel a shutdown previously scheduled by [`SystemControl::shutdown`]. Idempotent:
    /// succeeds even if none is pending. Used by the curfew enforcer to undo a countdown
    /// when the window ends or curfew is disabled.
    fn abort_shutdown(&self) -> Result<(), ControlError>;
}

/// Controller for an **interactive** process (dev `run`, or the session helper): captures
/// the screen directly. On non-Windows this is the fake.
pub fn interactive_control() -> Arc<dyn SystemControl> {
    #[cfg(windows)]
    {
        Arc::new(windows::WindowsControl::new())
    }
    #[cfg(not(windows))]
    {
        Arc::new(FakeControl::new())
    }
}

/// Controller for the **SYSTEM service** (Session 0): process/kill/shutdown run directly,
/// but screenshots are delegated to a helper launched into the interactive session, since
/// Session 0 has no desktop to capture. On non-Windows this is the fake.
pub fn service_control() -> Arc<dyn SystemControl> {
    #[cfg(windows)]
    {
        Arc::new(service_control::ServiceControl::new())
    }
    #[cfg(not(windows))]
    {
        Arc::new(FakeControl::new())
    }
}
