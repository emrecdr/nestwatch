//! Behavioural tests for `run_rules_enforcer` **itself** — the loop, not the pure functions it
//! calls. Its own test binary so the `NESTWATCH_DATA_DIR` override is isolated, and so the
//! process-global enforcer heartbeat starts unstamped.
//!
//! # Why this binary exists
//!
//! Every property of that loop was pinned by *source scans* — tests that `include_str!` their own
//! file and match strings against it. They earn their place for properties no unit test can see
//! (where a call site sits relative to an early `continue`), but they are not free: they fail with
//! messages about text rather than behaviour. When the stand-down condition was last mutated, the
//! first thing to go red said only "the stand-down branch must exist", which is true of the mutant
//! and says nothing about the harm.
//!
//! They also could not catch the defect that prompted this file. `Rules::any_configured()` folded
//! "is it paused" together with "is anything configured", and the loop stood the whole tick down
//! for both — which skips *measurement*, not just enforcement. A fresh install is enabled with
//! nothing configured, so every new install counted no screen time at all while `doctor` and the
//! dashboard both reported that it was counting. Every unit test passed throughout, because every
//! unit was correct.
//!
//! # How it drives a 30-second loop in milliseconds
//!
//! `tokio::time::pause()` is not usable here: the loop measures elapsed time with
//! `std::time::Instant`, which tokio's clock does not virtualise, so a paused clock would tick the
//! loop while charging it zero seconds. It does not need to be paused. `tokio::time::interval`
//! fires its **first** tick immediately, and the property under test needs no elapsed time — a
//! measuring tick clears the day's paused mark even when the interval it charges is zero, and a
//! stood-down tick sets it and measures nothing. So each case spawns the loop, waits for the
//! observable that first tick produces, and aborts.

use std::path::Path;
use std::sync::{Arc, RwLock};

use nestwatch::config::data_paths;
use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::rules::{Rules, Usage, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, idle_waker, test_config, wait_for};

/// Start the enforcer with `rules` and nothing else changed. Returns the handle so the caller can
/// abort it; the loop never returns on its own.
fn spawn_enforcer(rules: Rules) -> tokio::task::JoinHandle<()> {
    let mut cfg = test_config();
    cfg.rules = rules;
    let control: Arc<dyn SystemControl> = Arc::new(FakeControl::new());
    tokio::spawn(run_rules_enforcer(
        control,
        Arc::new(RwLock::new(cfg)),
        // Disabled: this binary asserts on the tally sidecar, which is the observable the defect
        // actually moved. The event logs are `record`-and-forget and would only add flake.
        Arc::new(UsageLog::disabled()),
        Arc::new(ScreentimeLog::disabled()),
        // The production ceiling over an empty status map: no probe scheduler in this binary, so
        // no provider can be reported as un-checkable — the same answer an install with no
        // integrations gives.
        nestwatch::probe::gate_ceiling(Default::default()),
        Feed::new(),
        idle_waker(),
    ))
}

fn tally_path() -> std::path::PathBuf {
    data_paths().dir.join("usage_state.json")
}

fn tally_in(path: &Path) -> Option<Usage> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The whole point of `TickMode`, observed through the loop rather than argued from its source.
///
/// One test rather than two, because `NESTWATCH_DATA_DIR` is process-global and two `#[tokio::test]`
/// functions in one binary run concurrently by default — they would race over the same override.
/// Sequencing them here also makes the paused phase carry its own weight: it leaves a tally marked
/// as not measured, so the measuring phase's "the mark is gone" can only come from a tick that
/// measured.
#[tokio::test]
async fn a_paused_tick_measures_nothing_and_an_unconfigured_one_still_measures() {
    let tmp = ScratchDir::new("enforcer-loop");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };
    let tally = tally_path();

    assert!(
        nestwatch::heartbeat::worst_age_secs().is_none(),
        "a fresh binary must start with no enforcer heartbeat, or the paused phase below \
         cannot tell 'the tick ran' from 'a previous test ran'"
    );
    assert!(!tally.exists(), "the scratch data dir must start empty");

    // ---- Paused: the tick must reach the loop and then measure nothing ----------------------
    let paused = spawn_enforcer(Rules {
        enabled: false,
        // Configured, so this is unambiguously the pause and not an empty rule set.
        daily_budget_mins: 60,
        ..Default::default()
    });

    assert!(
        wait_for(|| nestwatch::heartbeat::worst_age_secs().is_some()).await,
        "the paused loop never stamped a heartbeat — it must keep reporting itself alive, or \
         pausing would make the dashboard show enforcement as dead within two minutes"
    );
    // A stood-down tick does write the tally: the day still turns while paused, and what was
    // refused is kept for the card (`RulesEnforcer::stand_down`). What it must never write is a
    // measurement. Pause is the control that means "stop watching him", so the record of a paused
    // day has to be a gap, not a quieter kind of surveillance.
    assert!(
        wait_for(|| tally_in(&tally).is_some()).await,
        "the paused tick never saved the day it turned onto"
    );
    let paused_day = tally_in(&tally).expect("the tally");
    assert_eq!(
        paused_day.total_secs, 0,
        "a paused tick charged screen time"
    );
    for (what, empty) in [
        ("per-app", paused_day.per_app_secs.is_empty()),
        ("per-group", paused_day.per_group_secs.is_empty()),
        ("focus", paused_day.foreground_secs.is_empty()),
        ("page", paused_day.page_secs.is_empty()),
    ] {
        assert!(empty, "a paused tick recorded {what} time: {paused_day:?}");
    }
    assert!(
        paused_day.unmeasured,
        "the paused day is not marked as unmeasured, so its rollover would write a row of zeros \
         and the report would show a day nobody watched as a day nobody used the PC"
    );
    paused.abort();

    // ---- Enabled with nothing configured: the tick must measure ----------------------------
    let measuring = spawn_enforcer(Rules::default()); // a fresh install, exactly as shipped
    let measured = wait_for(|| tally_in(&tally).is_some_and(|u| !u.unmeasured)).await;
    measuring.abort();

    assert!(
        measured,
        "an enabled install with no rules yet never measured the day: the tally still carries the \
         paused mark, so no tick reached `decide`. This is the defect `TickMode::Measure` exists \
         to prevent: it is the state every install starts in, the dashboard calls it \"tracking \
         only\", and `doctor` calls it \"counting screen time\"."
    );
}
