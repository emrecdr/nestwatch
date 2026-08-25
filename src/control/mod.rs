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

/// A running process as the **enforcement tick** needs to see it: what it is, and how to stop it.
///
/// Deliberately narrower than [`ProcessInfo`], and a separate type rather than that one with a
/// field left at zero. The two are gathered at very different costs, and the type is what keeps
/// them apart.
///
/// [`SystemControl::list_processes`] renders a panel a parent opens occasionally, so it can afford
/// a memory figure. This one runs every `CHECK_INTERVAL` for the life of the machine and reads
/// exactly these two fields. A single type carrying an unused `memory_bytes` is how the cheap scan
/// came to be paying for the expensive number in the first place — leaving the field present, even
/// zeroed, invites that back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningProcess {
    pub pid: u32,
    pub name: String,
}

/// Whether an interactive user is present at the console — drives screen-time accounting so the
/// budget isn't charged while nobody is using the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// No user is logged in at the console (e.g. the machine is at the sign-in screen, or off).
    /// Nothing to charge time to.
    NoUser,
    /// A user is logged in but the workstation is locked (lock screen / screensaver). Present,
    /// but not actively using the machine.
    Locked,
    /// A user is logged in and the session is unlocked — actively usable.
    Active,
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

    /// List currently running processes with their memory use, for the dashboard's process panel.
    /// Called on demand, when a parent opens that card.
    fn list_processes(&self) -> Result<Vec<ProcessInfo>, ControlError>;

    /// The pid and name of every running process — the enforcement tick's view, and nothing more.
    ///
    /// Split from [`list_processes`](Self::list_processes) because the two run on completely
    /// different schedules and the difference is not free. On Windows the memory figure costs a
    /// kernel call per process, and the default refresh that used to serve both callers also
    /// computed CPU percentage, disk-I/O counters and the full executable path for every process
    /// on the machine — several hundred of them, twice a minute, forever, all discarded here.
    ///
    /// Ordering is unspecified: the enforcer folds the result into a set.
    fn running_processes(&self) -> Result<Vec<RunningProcess>, ControlError>;

    /// Terminate the process with the given PID.
    fn kill_process(&self, pid: u32) -> Result<(), ControlError>;

    /// Lock the interactive session (require the user's password to resume). Softer than a
    /// shutdown: nothing is powered off and no work is lost.
    fn lock_workstation(&self) -> Result<(), ControlError>;

    /// Begin an orderly shutdown of the machine after `delay_secs`, optionally showing the
    /// user a warning `message` during the countdown.
    fn shutdown(&self, delay_secs: u32, message: Option<String>) -> Result<(), ControlError>;

    /// Cancel a shutdown previously scheduled by [`SystemControl::shutdown`]. Idempotent:
    /// succeeds even if none is pending. Used by the curfew enforcer to undo a countdown
    /// when the window ends or curfew is disabled.
    fn abort_shutdown(&self) -> Result<(), ControlError>;

    /// Report whether an interactive user is present and actively using the console session.
    /// The screen-time enforcer consults this so it doesn't charge the daily budget while
    /// nobody is logged in or the screen is locked. Best-effort: the enforcer treats an `Err`
    /// as [`SessionState::Active`] — failing toward enforcement, never toward unlimited time.
    fn session_state(&self) -> Result<SessionState, ControlError>;

    /// Show the interactive user a brief notification — used to warn the child before a
    /// screen-time lock, or when a Warn-mode limit is reached, so enforcement isn't a silent
    /// surprise. Best-effort and **non-blocking**: it returns immediately (the message
    /// auto-dismisses) and never waits for the user to click.
    fn notify_user(&self, title: String, body: String) -> Result<(), ControlError>;
}

/// Show the interactive user a notification from an async context, off the runtime.
///
/// Lives here rather than in either enforcer because both need it: the rules enforcer warns
/// before a screen-time lock, curfew before bedtime. (The trait method stays synchronous like
/// every other one — this is just the `spawn_blocking` wrapper around it.)
///
/// Returns **whether the OS accepted the message**, so callers can record warnings that were
/// actually delivered rather than ones that were merely intended. That distinction is the same
/// one the enforcers already make for locks and shutdowns: a notification can fail for reasons
/// nobody would otherwise see (a stale console session id, fast-user-switching, no interactive
/// user), and a countdown that silently never reaches the child looks identical in the logs to
/// one that did. Failure is never fatal — a missed warning must not stall or crash an enforcer.
pub async fn notify(control: &Arc<dyn SystemControl>, title: &str, body: &str) -> bool {
    let control = control.clone();
    let (title, body) = (title.to_string(), body.to_string());
    match tokio::task::spawn_blocking(move || control.notify_user(title, body)).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "child notification failed");
            false
        }
        Err(e) => {
            tracing::error!(error = %e, "child notification task panicked");
            false
        }
    }
}

/// Encode a decoded image as PNG bytes. Shared by the real and fake controllers so the
/// buffer/format/error-mapping lives in one place (child modules see this private helper).
fn encode_png(img: image::DynamicImage) -> Result<Vec<u8>, ControlError> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| ControlError::Capture(e.to_string()))?;
    Ok(buf.into_inner())
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
