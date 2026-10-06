//! A locked screen accrues no screen time — observed through `run_rules_enforcer`, the loop.
//!
//! Its own test binary for the `NESTWATCH_DATA_DIR` override, as `enforcer_loop.rs` explains.
//!
//! Written for a survivor of the first full mutation sweep (`docs/OPEN-FINDINGS.md` O109):
//! deleting the loop's `Ok(Ok(state)) => matches!(state, SessionState::Active)` arm sends every
//! answer to the fallback for a *failed* query, which counts as active — so a locked screen was
//! charged as use, and every test still passed. They passed because the loop tests only ever ran
//! one tick, and the first tick charges no time whatever the session is doing.
//!
//! So this runs two: the first immediately, the second woken through the enforcers' own wake
//! channel a little over a second later, which charges that second if and only if somebody is at
//! the machine. The active phase is the control. Without it, a loop that charged nothing at all
//! would pass the locked phase for the wrong reason.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use nestwatch::config::{Config, data_paths};
use nestwatch::control::{FakeControl, SessionState, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::rules::run_rules_enforcer;
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, wait_for};

fn tally_path() -> PathBuf {
    data_paths().dir.join("usage_state.json")
}

fn total_secs() -> Option<u64> {
    let raw = std::fs::read_to_string(tally_path()).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw).ok()?["total_secs"].as_u64()
}

#[tokio::test]
async fn a_locked_screen_accrues_no_screen_time_and_an_active_one_does() {
    let tmp = ScratchDir::new("enforcer-session");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    for (state, charged) in [(SessionState::Locked, false), (SessionState::Active, true)] {
        let _ = std::fs::remove_file(tally_path());
        let fake = Arc::new(FakeControl::new());
        fake.script_session_state(Ok(state));
        let control: Arc<dyn SystemControl> = fake;
        let (waker, wake) = tokio::sync::watch::channel(0u64);
        let enforcer = tokio::spawn(run_rules_enforcer(
            control,
            // A fresh install: measuring, nothing to enforce. No password either — the enforcer
            // never reads one, and `test_config` would hash it on every phase.
            Arc::new(RwLock::new(Config::default())),
            Arc::new(UsageLog::disabled()),
            Arc::new(ScreentimeLog::disabled()),
            nestwatch::probe::gate_ceiling(Default::default()),
            Feed::new(),
            wake,
        ));

        assert!(
            wait_for(|| total_secs().is_some()).await,
            "{state:?}: the first tick never wrote the tally"
        );
        // The first tick stamped its clock before writing the tally just seen, so a second from
        // here is a whole second for the next tick to charge.
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        nestwatch::heartbeat::wake(&waker);

        if charged {
            assert!(
                wait_for(|| total_secs().is_some_and(|secs| secs >= 1)).await,
                "an active session was charged nothing for the second between two ticks — the \
                 setup cannot accrue, so the locked phase above proved nothing"
            );
        } else {
            // Nothing changes on this path, so there is no write to wait for; the margin only
            // covers a descheduled task reaching the tick at all, as in `enforcer_loop.rs`.
            tokio::time::sleep(Duration::from_millis(250)).await;
            assert_eq!(
                total_secs(),
                Some(0),
                "a locked screen was charged as screen time"
            );
        }
        enforcer.abort();
    }
}
