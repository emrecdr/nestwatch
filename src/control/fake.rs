//! A deterministic, side-effect-free [`SystemControl`] for macOS development and tests.
//!
//! It keeps an in-memory process list (so "kill" visibly removes an entry), synthesises a
//! placeholder JPEG for screenshots, and makes "shutdown" a logged no-op — so you can
//! exercise every endpoint and the full UI without a Windows box or real side effects.
//!
//! One method is not side-effect-free, and it is named: [`SystemControl::run_probe`] runs the
//! file it is given unless a test scripted an answer. That is the dev path for a provider probe,
//! and it only ever runs a file a parent configured.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{ControlError, ProcessInfo, RunningProcess, SessionState, ShotTier, SystemControl};

/// How many shutdown requests [`FakeControl`] remembers. Far above what any test asserts on, and
/// small enough that a dev server left running for weeks cannot grow a list out of it.
const SHUTDOWN_LOG_CAP: usize = 64;

/// Same bound, same reason, for [`FakeControl::notifications`]. Higher than shutdowns because a
/// day's worth of countdown warnings is legitimately more numerous than a day's shutdowns.
const NOTIFY_LOG_CAP: usize = 128;

/// Same bound, same reason, for [`FakeControl::probe_calls`]. A probe runs a few times an hour,
/// so this is weeks of a dev server left up.
const PROBE_LOG_CAP: usize = 128;

pub struct FakeControl {
    processes: Mutex<Vec<ProcessInfo>>,
    /// Every `(delay_secs, message)` this fake was asked to shut down with, in order.
    ///
    /// Recorded rather than merely logged because the message is the *only* thing the child sees
    /// on a Shutdown-configured install — `shutdown.exe /c "…"` is the whole notification, with no
    /// toast beside it. Two real defects lived in that string with nothing able to observe it: it
    /// was hard-coded English on every install, and it was the one child-facing message that never
    /// carried the "where to ask for more time" address. A `tracing::warn!` cannot be asserted on,
    /// so nothing could reach the loop that builds it. `tests/enforcer_shutdown.rs` and
    /// `tests/curfew_enforcer.rs` now do, through this field; `docs/OPEN-FINDINGS.md` O70 tracks
    /// what a driver test still cannot see, which is the *order* the loop does things in.
    shutdowns: Mutex<Vec<(u32, Option<String>)>>,
    /// Every `(title, body)` this fake was asked to show the child, in order.
    ///
    /// **Recorded for exactly the reason `shutdowns` above is, one argument later.** That field's
    /// doc used to end by naming what was still invisible — *"`notify_user` is still only a log
    /// line, so the warnings remain unassertable"* — and that sentence covered nearly everything
    /// this product ever says to a child: every 15/5/1 countdown, the lock grace notice, the
    /// limit-reached notice, and the app-stopped notice. A `tracing::info!` cannot be asserted on,
    /// so no test anywhere could show that a warning was delivered, that it was in the child's
    /// language, or that it carried the address to ask at.
    ///
    /// The two defects that made `shutdowns` observable — a hard-coded English string, and the one
    /// child-facing message that never carried the "where to ask" hint — were both *found* on the
    /// shutdown path, and both had a live twin on this one that nothing could reach.
    /// `tests/translated_strings.rs` guards the shape of these messages statically; this is what
    /// lets a test assert that one actually arrived, and with what in it.
    notifications: Mutex<Vec<(String, String)>>,
    /// What [`SystemControl::session_state`] answers. Active by default, which is what every test
    /// written before this field existed assumes and what a dev server wants — the screen-time
    /// enforcer accrues time exactly as it did. Scriptable because two behaviours are decided by
    /// this answer and neither could be reached otherwise: the enforcer treats an unreadable state
    /// as `Active`, so a failure never hands out unlimited time, and `probe::run_once` treats it as
    /// "do not run", so a failure never spends a request on a third party.
    session: Mutex<Result<SessionState, String>>,
    /// What [`SystemControl::notify_user`] answers. `Ok` by default, which is what every test
    /// written before this field existed assumes. Scriptable because two callers ask whether the
    /// OS actually took the message before recording that the child was told, and a fake that can
    /// only succeed leaves that branch unreachable.
    notify_result: Mutex<Result<(), String>>,
    /// A scripted answer for [`SystemControl::run_probe`], or `None` to run the file for real.
    probe_script: Mutex<Option<Result<Vec<u8>, String>>>,
    /// Every `(exe, input)` this fake was asked to probe with, in order — so a test can assert
    /// the caller's half of the contract: which file, with which secret.
    probe_calls: Mutex<Vec<(PathBuf, Vec<u8>)>>,
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
            notifications: Mutex::new(Vec::new()),
            session: Mutex::new(Ok(SessionState::Active)),
            notify_result: Mutex::new(Ok(())),
            probe_script: Mutex::new(None),
            probe_calls: Mutex::new(Vec::new()),
        }
    }

    /// Make every later [`SystemControl::notify_user`] fail with `error`, as an OS that has no
    /// interactive session to show a box on does.
    pub fn fail_notifications(&self, error: &str) {
        *self
            .notify_result
            .lock()
            .expect("fake notify result poisoned") = Err(error.to_string());
    }

    /// Answer every later [`SystemControl::session_state`] with `answer`.
    pub fn script_session_state(&self, answer: Result<SessionState, String>) {
        *self.session.lock().expect("fake session state poisoned") = answer;
    }

    /// Answer every later [`SystemControl::run_probe`] with `answer` instead of running the file.
    pub fn script_probe(&self, answer: Result<Vec<u8>, String>) {
        *self
            .probe_script
            .lock()
            .expect("fake probe script poisoned") = Some(answer);
    }

    /// Every probe this fake was asked to run, as `(exe, input)`, oldest first.
    pub fn probe_calls(&self) -> Vec<(PathBuf, Vec<u8>)> {
        self.probe_calls
            .lock()
            .expect("fake probe log poisoned")
            .clone()
    }

    /// Every shutdown this fake was asked for, as `(delay_secs, message)`, oldest first.
    pub fn shutdowns(&self) -> Vec<(u32, Option<String>)> {
        self.shutdowns
            .lock()
            .expect("fake shutdown log poisoned")
            .clone()
    }

    /// Every notification this fake was asked to show, as `(title, body)`, oldest first.
    pub fn notifications(&self) -> Vec<(String, String)> {
        self.notifications
            .lock()
            .expect("fake notification log poisoned")
            .clone()
    }

    /// The bodies of those notifications, which is what every caller actually asserts on — the
    /// title is a constant. Saves each test writing the same `.into_iter().map(|(_, b)| b)`.
    pub fn notification_bodies(&self) -> Vec<String> {
        self.notifications()
            .into_iter()
            .map(|(_, body)| body)
            .collect()
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
        // Dev/tests: a user is actively at the machine unless a test said otherwise, so the
        // screen-time enforcer accrues time exactly as it did before this was scriptable.
        self.session
            .lock()
            .expect("fake session state poisoned")
            .clone()
            .map_err(ControlError::Op)
    }

    fn notify_user(&self, title: String, body: String) -> Result<(), ControlError> {
        tracing::info!(%title, %body, "[fake] notify_user (no-op on this platform)");
        // Recorded before the scripted result is consulted: a message the OS refused was still
        // *attempted*, and a test asserting that nothing was said must be able to tell the two
        // apart.
        let scripted = self
            .notify_result
            .lock()
            .expect("fake notify result poisoned")
            .clone();
        // Capped keeping the OLDEST, for the same reason `shutdown` above gives: assertions index
        // from the front, so dropping from the front would silently renumber what a test reads.
        let mut log = self
            .notifications
            .lock()
            .expect("fake notification log poisoned");
        if log.len() < NOTIFY_LOG_CAP {
            log.push((title, body));
        }
        scripted.map_err(ControlError::Op)
    }

    /// Scripted when a test asked for it; otherwise the file is genuinely run, as this user. That
    /// is the one side effect this fake has, and it is the dev path: `nestwatch run` on a Mac with
    /// a compiled probe in the data dir exercises the whole loop without a Windows box.
    fn run_probe(&self, exe: &Path, input: Vec<u8>) -> Result<Vec<u8>, ControlError> {
        {
            let mut calls = self.probe_calls.lock().expect("fake probe log poisoned");
            if calls.len() < PROBE_LOG_CAP {
                calls.push((exe.to_path_buf(), input.clone()));
            }
        }
        let scripted = self
            .probe_script
            .lock()
            .expect("fake probe script poisoned")
            .clone();
        match scripted {
            Some(answer) => answer.map_err(ControlError::Op),
            None => {
                super::run_local_probe(exe, &input, super::PROBE_TIMEOUT, super::MAX_PROBE_OUTPUT)
            }
        }
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

    /// The log is bounded, and it is bounded at the **front**.
    ///
    /// Both halves matter and neither is visible from the call site. `control::new()` hands this
    /// type to the real server on every non-Windows build, where `shutdown` is a no-op — so the
    /// machine never powers off, the curfew enforcer re-issues for the whole window, and an
    /// unbounded `Vec` grows all night in a process nothing restarts. That is the cap.
    ///
    /// Which end it drops is the part a future tidy-up would get wrong. A ring buffer keeping the
    /// *newest* is the more usual shape, and it would leave every existing assertion still
    /// compiling while silently renumbering what `shutdowns()[0]` means — the first shutdown a
    /// test asserts on becomes whichever one happened to survive.
    /// The notification log is bounded the same way, and for the same reason: a dev server left
    /// running for weeks must not grow a list out of the fake. Separate from the shutdown test
    /// because the two caps are separate constants, and a shared test would keep passing if one
    /// of them were removed.
    #[test]
    fn the_notification_log_is_capped_and_keeps_the_oldest() {
        let c = FakeControl::new();
        for i in 0..(NOTIFY_LOG_CAP + 10) {
            c.notify_user("Screen time".into(), format!("notice {i}"))
                .unwrap();
        }

        let log = c.notification_bodies();
        assert_eq!(log.len(), NOTIFY_LOG_CAP, "the log grew past its cap");
        assert_eq!(
            log[0], "notice 0",
            "the front of the log moved — assertions that index from it now read a different call"
        );
        assert_eq!(
            log[NOTIFY_LOG_CAP - 1],
            format!("notice {}", NOTIFY_LOG_CAP - 1),
            "the retained window is not the first {NOTIFY_LOG_CAP} calls"
        );
    }

    #[test]
    fn the_shutdown_log_is_capped_and_keeps_the_oldest() {
        let c = FakeControl::new();
        for i in 0..(SHUTDOWN_LOG_CAP as u32 + 10) {
            c.shutdown(i, Some(format!("notice {i}"))).unwrap();
        }

        let log = c.shutdowns();
        assert_eq!(log.len(), SHUTDOWN_LOG_CAP, "the log grew past its cap");
        assert_eq!(
            log[0],
            (0, Some("notice 0".to_string())),
            "the front of the log moved — assertions that index from it now read a different call"
        );
        assert_eq!(
            log[SHUTDOWN_LOG_CAP - 1].0,
            SHUTDOWN_LOG_CAP as u32 - 1,
            "the retained window is not the first {SHUTDOWN_LOG_CAP} calls"
        );
    }

    /// A scripted answer is what a test gets, and the fake remembers what it was asked so the
    /// caller's half of the contract — which file, with which secret — is assertable.
    #[test]
    fn a_scripted_probe_answers_and_records_what_it_was_asked() {
        let fake = FakeControl::new();
        fake.script_probe(Ok(br#"{"questions":12,"minutes":24}"#.to_vec()));
        let out = fake
            .run_probe(
                std::path::Path::new("studygo-probe.exe"),
                b"the secret".to_vec(),
            )
            .unwrap();
        assert_eq!(out, br#"{"questions":12,"minutes":24}"#);
        assert_eq!(
            fake.probe_calls(),
            vec![(
                std::path::PathBuf::from("studygo-probe.exe"),
                b"the secret".to_vec()
            )]
        );

        fake.script_probe(Err("no network".into()));
        let err = fake
            .run_probe(std::path::Path::new("studygo-probe.exe"), Vec::new())
            .unwrap_err();
        assert!(err.to_string().contains("no network"), "got {err}");
        assert_eq!(fake.probe_calls().len(), 2, "a failure is a call too");
    }

    /// Unscripted, the fake runs the file for real — the dev path on a Mac — so a compiled probe
    /// can be exercised end to end without a Windows box.
    #[cfg(unix)]
    #[test]
    fn an_unscripted_probe_runs_the_file_and_hands_it_the_secret() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::testutil::ScratchDir::new("fake-probe");
        let path = dir.join("echo-back");
        std::fs::write(&path, "#!/bin/sh\ncat\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = FakeControl::new()
            .run_probe(&path, b"hello".to_vec())
            .unwrap();
        assert_eq!(out, b"hello");
    }

    /// The probe log keeps the oldest `PROBE_LOG_CAP` calls and not one more — the same bound,
    /// for the same reason, as the shutdown log above. Mutation testing found the comparison
    /// one-sided: `<` and `<=` both keep the first call, and only counting tells them apart.
    #[test]
    fn the_probe_log_is_capped_at_the_oldest_calls() {
        let fake = FakeControl::new();
        fake.script_probe(Ok(b"{}".to_vec()));
        for i in 0..PROBE_LOG_CAP + 5 {
            let _ = fake.run_probe(std::path::Path::new("probe"), i.to_string().into_bytes());
        }
        let log = fake.probe_calls();
        assert_eq!(log.len(), PROBE_LOG_CAP, "the cap must hold exactly");
        assert_eq!(log[0].1, b"0", "the oldest call is the one kept");
        assert_eq!(
            log[PROBE_LOG_CAP - 1].1,
            (PROBE_LOG_CAP - 1).to_string().into_bytes(),
            "the retained window is the first {PROBE_LOG_CAP} calls"
        );
    }
}
