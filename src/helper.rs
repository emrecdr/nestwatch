//! The `helper --capture <path>` subcommand.
//!
//! Launched by the SYSTEM service into the interactive user session (see `crate::session`),
//! it runs with a desktop, captures the screen with the direct controller, writes a PNG to
//! `path`, and exits. The service then reads that file.

use anyhow::{Context, Result};

pub fn capture(path: &str) -> Result<()> {
    let control = crate::control::interactive_control();
    let png = control
        .screenshot_png()
        .map_err(|e| anyhow::anyhow!(e.to_string()))
        .context("screen capture failed")?;
    std::fs::write(path, png).with_context(|| format!("writing screenshot to {path}"))?;
    Ok(())
}
