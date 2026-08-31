//! A deterministic, side-effect-free [`SystemControl`] for macOS development and tests.
//!
//! It keeps an in-memory process list (so "kill" visibly removes an entry), synthesises a
//! placeholder JPEG for screenshots, and makes "shutdown" a logged no-op — so you can
//! exercise every endpoint and the full UI without a Windows box or real side effects.

use std::sync::Mutex;

use super::{ControlError, ProcessInfo, RunningProcess, SessionState, ShotTier, SystemControl};

/// How many shutdown requests [`FakeControl`] remembers. Far above what any test asserts on, and
/// small enough that a dev server left running for weeks cannot grow a list out of it.
const SHUTDOWN_LOG_CAP: usize = 64;

pub struct FakeControl {
    processes: Mutex<Vec<ProcessInfo>>,
    /// Every `(delay_secs, message)` this fake was asked to shut down with, in order.
    ///
    /// Recorded rather than merely logged because the message is the *only* thing the child sees
    /// on a Shutdown-configured install — `shutdown.exe /c "…"` is the whole notification, with no
    /// toast beside it. Two real defects lived in that string with nothing able to observe it: it
    /// was hard-coded English on every install, and it was the one child-facing message that never
    /// carried the "where to ask for more time" address. A `tracing::warn!` cannot be asserted on,
    /// so the loop that builds it had no test at all (`docs/OPEN-FINDINGS.md` O70).
    shutdowns: Mutex<Vec<(u32, Option<String>)>>,
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
            shutdowns: Mutex::new(Vec::new()),
        }
    }

    /// Every shutdown this fake was asked for, as `(delay_secs, message)`, oldest first.
    pub fn shutdowns(&self) -> Vec<(u32, Option<String>)> {
        self.shutdowns
            .lock()
            .expect("fake shutdown log poisoned")
            .clone()
    }
}

impl Default for FakeControl {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemControl for FakeControl {
    /// A diagonal gradient, so the UI has something real to display and the tiers are
    /// distinguishable.
    ///
    /// Deliberately **larger than `PREVIEW_W`×`PREVIEW_H`** — it used to be
    /// 320×180. A source smaller than the preview box is returned untouched by `encode_shot`, so
    /// with the old size both tiers produced identical bytes and no test could tell whether the
    /// tier had reached the implementation at all. 1280×720 is the smallest ordinary desktop shape
    /// that actually exercises the downscale.
    ///
    /// **`Rgba8`, because that is what the shipping controller produces.** `windows.rs` always
    /// hands `encode_shot` an `ImageRgba8`, and `encode_shot` matches on the variant — so a fake
    /// producing `Rgb8` sent every test down the arm production never takes, and left the arm it
    /// does take covered by a single bespoke unit test. Worse, the fallback arm carries a
    /// full-frame `into_rgb8()` copy, so the size assertions measured on it were measuring a path
    /// with different costs from the real one. Alpha is a constant 255: a desktop capture is
    /// opaque, and JPEG discards the channel anyway.
    fn screenshot(&self, tier: ShotTier) -> Result<Vec<u8>, ControlError> {
        let (w, h) = (1280u32, 720u32);
        let mut img = image::RgbaImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, 128, 255]);
        }
        super::encode_shot(image::DynamicImage::ImageRgba8(img), tier)
    }

    fn list_processes(&self) -> Result<Vec<ProcessInfo>, ControlError> {
        Ok(self.processes.lock().unwrap().clone())
    }

    /// Projected from the same list `list_processes` returns, never a second one. A `kill` is then
    /// visible through both views, and the fake cannot drift into disagreeing with itself about
    /// what is running — which would let a test pass against a world the real implementations
    /// cannot produce.
    fn running_processes(&self) -> Result<Vec<RunningProcess>, ControlError> {
        Ok(self
            .processes
            .lock()
            .unwrap()
            .iter()
            .map(|p| RunningProcess {
                pid: p.pid,
                name: p.name.clone(),
            })
            .collect())
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
        // Bounded, because this type is not only a test double: `control::new()` hands the real
        // server a `FakeControl` on every non-Windows build, so a dev machine left running with a
        // curfew accumulates these for the life of the process. And it accumulates *fast* — the
        // shutdown here is a no-op, so the machine never powers off, so the curfew enforcer keeps
        // re-issuing `ShutdownNow` for the whole window rather than the once a real shutdown would
        // allow. Unbounded growth in a long-lived process for a log nothing in production reads.
        //
        // Keeps the **oldest**, not the newest: assertions index from the front, so a cap that
        // dropped from the front would silently renumber what a test is looking at.
        let mut log = self.shutdowns.lock().expect("fake shutdown log poisoned");
        if log.len() < SHUTDOWN_LOG_CAP {
            log.push((delay_secs, message));
        }
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

    fn notify_user(&self, title: String, body: String) -> Result<(), ControlError> {
        tracing::info!(%title, %body, "[fake] notify_user (no-op on this platform)");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two process views must describe the same machine.
    ///
    /// `list_processes` and `running_processes` exist separately because they cost very different
    /// amounts to gather — but they answer the same question, and a test that passed against a fake
    /// where they disagreed would be testing a world neither real implementation can produce. The
    /// kill is the part that matters: it is the one operation that changes the answer, and a fake
    /// projecting the second view from a stale copy would keep reporting a process the parent had
    /// already stopped.
    #[test]
    fn both_process_views_agree_before_and_after_a_kill() {
        let c = FakeControl::new();

        let listed = |c: &FakeControl| -> Vec<(u32, String)> {
            let mut v: Vec<_> = c
                .list_processes()
                .unwrap()
                .into_iter()
                .map(|p| (p.pid, p.name))
                .collect();
            v.sort();
            v
        };
        let running = |c: &FakeControl| -> Vec<(u32, String)> {
            let mut v: Vec<_> = c
                .running_processes()
                .unwrap()
                .into_iter()
                .map(|p| (p.pid, p.name))
                .collect();
            v.sort();
            v
        };

        assert_eq!(listed(&c), running(&c), "views disagree before any change");
        assert!(!listed(&c).is_empty(), "the fake starts with processes");

        let victim = c.list_processes().unwrap()[0].pid;
        c.kill_process(victim).unwrap();

        assert_eq!(listed(&c), running(&c), "views disagree after a kill");
        assert!(
            running(&c).iter().all(|(pid, _)| *pid != victim),
            "the killed process is still reported as running"
        );
    }
}
