//! A deterministic, side-effect-free [`SystemControl`] for macOS development and tests.
//!
//! It keeps an in-memory process list (so "kill" visibly removes an entry), synthesises a
//! small placeholder PNG for screenshots, and makes "shutdown" a logged no-op — so you can
//! exercise every endpoint and the full UI without a Windows box or real side effects.

use std::sync::Mutex;

use super::{ControlError, ProcessInfo, SessionState, SystemControl};

pub struct FakeControl {
    processes: Mutex<Vec<ProcessInfo>>,
}

impl FakeControl {
    pub fn new() -> Self {
        Self {
            processes: Mutex::new(vec![
                ProcessInfo {
                    pid: 1001,
                    name: "explorer.exe".into(),
                    memory_bytes: 45_000_000,
                },
                ProcessInfo {
                    pid: 1002,
                    name: "chrome.exe".into(),
                    memory_bytes: 512_000_000,
                },
                ProcessInfo {
                    pid: 1003,
                    name: "Minecraft.exe".into(),
                    memory_bytes: 1_200_000_000,
                },
                ProcessInfo {
                    pid: 1004,
                    name: "Discord.exe".into(),
                    memory_bytes: 210_000_000,
                },
                ProcessInfo {
                    pid: 1005,
                    name: "notepad.exe".into(),
                    memory_bytes: 8_000_000,
                },
            ]),
        }
    }
}

impl Default for FakeControl {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemControl for FakeControl {
    fn screenshot_png(&self) -> Result<Vec<u8>, ControlError> {
        // A 320x180 diagonal gradient so the UI has something real to display.
        let (w, h) = (320u32, 180u32);
        let mut img = image::RgbImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, 128]);
        }
        super::encode_png(image::DynamicImage::ImageRgb8(img))
    }

    fn list_processes(&self) -> Result<Vec<ProcessInfo>, ControlError> {
        Ok(self.processes.lock().unwrap().clone())
    }

    fn kill_process(&self, pid: u32) -> Result<(), ControlError> {
        let mut procs = self.processes.lock().unwrap();
        let before = procs.len();
        procs.retain(|p| p.pid != pid);
        if procs.len() == before {
            return Err(ControlError::ProcessNotFound(pid));
        }
        Ok(())
    }

    fn shutdown(&self, delay_secs: u32, message: Option<String>) -> Result<(), ControlError> {
        tracing::warn!(
            delay_secs,
            ?message,
            "[fake] shutdown requested (no-op on this platform)"
        );
        Ok(())
    }

    fn abort_shutdown(&self) -> Result<(), ControlError> {
        tracing::info!("[fake] abort_shutdown (no-op on this platform)");
        Ok(())
    }

    fn lock_workstation(&self) -> Result<(), ControlError> {
        tracing::info!("[fake] lock_workstation (no-op on this platform)");
        Ok(())
    }

    fn session_state(&self) -> Result<SessionState, ControlError> {
        // Dev/tests: pretend a user is actively at the machine, so the screen-time enforcer
        // accrues time exactly as it did before this method existed.
        Ok(SessionState::Active)
    }
}
