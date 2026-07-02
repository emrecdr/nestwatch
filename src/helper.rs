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
