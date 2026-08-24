//! The `helper --capture-stdout` / `helper --capture <path>` subcommand.
//!
//! Launched by the SYSTEM service into the interactive user session (see `crate::session`),
//! it runs with a desktop, captures the screen with the direct controller, and writes the
//! PNG to stdout (piped back to the service) or to a file. In stdout mode it must emit
//! *only* the PNG bytes — the caller does not initialize tracing for this subcommand.

use std::io::Write;

use anyhow::{Context, Result};

fn capture_png() -> Result<Vec<u8>> {
    let control = crate::control::interactive_control();
    control
        .screenshot_png()
        .map_err(|e| anyhow::anyhow!(e.to_string()))
        .context("screen capture failed")
}

/// Write the PNG to stdout (used by the service via a pipe).
pub fn capture_to_stdout() -> Result<()> {
    let png = capture_png()?;
    let mut out = std::io::stdout().lock();
    out.write_all(&png)
        .context("writing screenshot to stdout")?;
    out.flush().context("flushing stdout")?;
    Ok(())
}

/// Write the PNG to a file (handy for manual/dev use).
pub fn capture_to_file(path: &str) -> Result<()> {
    let png = capture_png()?;
    std::fs::write(path, png).with_context(|| format!("writing screenshot to {path}"))?;
    Ok(())
}

/// Lock the interactive session. Launched by the service as `helper --lock` inside the user's
/// session (see `crate::session::lock_active_session`), so `LockWorkStation` locks that desktop.
pub fn lock() -> Result<()> {
    crate::control::interactive_control()
        .lock_workstation()
        .map_err(|e| anyhow::anyhow!(e.to_string()))
        .context("lock failed")
}

/// Watch which app has focus, reporting a JSON line to stdout every 30 seconds.
///
/// Unlike the other three subcommands this one is **resident** — it runs for as long as the child
/// is signed in. See `crate::watcher` for why it cannot live in the service, and
/// `docs/FOREGROUND-TRACKING.md` for what it does and deliberately does not measure.
///
/// Like `--capture-stdout`, it owns stdout: tracing is not initialized for `helper`, so nothing
/// can interleave with the JSON lines the service is parsing.
pub fn watch() -> Result<()> {
    #[cfg(windows)]
    {
        crate::watcher::run().context("foreground watcher failed")
    }
    // Every other platform builds the whole server against `FakeControl`, and the watcher is the
    // one piece with no fake worth having: a foreground window on the developer's Mac says nothing
    // about the child's PC. Fail loudly rather than silently reporting an empty desktop forever.
    #[cfg(not(windows))]
    {
        anyhow::bail!("the foreground watcher is Windows-only")
    }
}
