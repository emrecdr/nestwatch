//! The real Windows implementation of [`SystemControl`].
//!
//! Compiled only on Windows (`#[cfg(windows)]` at the module declaration). Uses `xcap`
//! for screen capture, `sysinfo` for process enumeration/termination, and shells out to
//! `shutdown.exe` for power-off (dependency-free, no `unsafe`, no `windows` crate).

use super::{ControlError, ProcessInfo, SystemControl};

pub struct WindowsControl;

impl WindowsControl {
    pub fn new() -> Self {
        Self
    }
}

impl SystemControl for WindowsControl {
    fn screenshot_png(&self) -> Result<Vec<u8>, ControlError> {
        use xcap::Monitor;

        let monitor = Monitor::all()
            .map_err(|e| ControlError::Capture(e.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| ControlError::Capture("no monitor found".into()))?;

        let captured = monitor
            .capture_image()
            .map_err(|e| ControlError::Capture(e.to_string()))?;

        // Bridge via raw RGBA bytes so we don't couple to xcap's exact `image` version:
        // `into_raw()` yields a plain `Vec<u8>`, which we re-wrap with *our* `image` crate.
        let (width, height) = (captured.width(), captured.height());
        let raw = captured.into_raw();
        let rgba = image::RgbaImage::from_raw(width, height, raw)
            .ok_or_else(|| ControlError::Capture("unexpected frame buffer size".into()))?;

        super::encode_png(image::DynamicImage::ImageRgba8(rgba))
    }

    fn list_processes(&self) -> Result<Vec<ProcessInfo>, ControlError> {
        use sysinfo::{ProcessesToUpdate, System};

        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);

        let mut out: Vec<ProcessInfo> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| ProcessInfo {
                pid: pid.as_u32(),
                name: proc_.name().to_string_lossy().into_owned(),
                memory_bytes: proc_.memory(),
            })
            .collect();
        // Heaviest first — the apps a parent most likely wants to see/close.
        out.sort_by_key(|p| std::cmp::Reverse(p.memory_bytes));
        Ok(out)
    }

    fn kill_process(&self, pid: u32) -> Result<(), ControlError> {
        use sysinfo::{Pid, ProcessesToUpdate, System};

        // Refresh only the target PID rather than walking the whole process table.
        let target = Pid::from_u32(pid);
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::Some(&[target]), true);

        let Some(proc_) = sys.process(target) else {
            return Err(ControlError::ProcessNotFound(pid));
        };
        if proc_.kill() {
            Ok(())
        } else {
            Err(ControlError::Op(format!("failed to kill pid {pid}")))
        }
    }

    fn shutdown(&self, delay_secs: u32, message: Option<String>) -> Result<(), ControlError> {
        // `/t N` gives Windows' own countdown; `/c "msg"` shows the user a reason.
        let delay = delay_secs.to_string();
        let mut cmd = std::process::Command::new("shutdown");
        cmd.args(["/s", "/t", &delay]);
        if let Some(msg) = message.as_deref() {
            // Windows truncates the comment at 512 chars.
            cmd.args(["/c", &msg.chars().take(512).collect::<String>()]);
        }
        let status = cmd.status().map_err(|e| ControlError::Op(e.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(ControlError::Op(format!("shutdown exited with {status}")))
        }
    }

    fn abort_shutdown(&self) -> Result<(), ControlError> {
        // `shutdown /a` cancels a pending shutdown; it exits non-zero ("no shutdown in
        // progress", 1116) when there is nothing to cancel — which is fine, so best-effort.
        std::process::Command::new("shutdown")
            .arg("/a")
            .output()
            .map_err(|e| ControlError::Op(e.to_string()))?;
        Ok(())
    }
}
