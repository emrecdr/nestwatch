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
//! fires its **first** tick immediately, and the property under test needs no elapsed time —
//! `usage_state.json` is written on a measuring tick and never reached on a stood-down one, and
//! `Usage::accrue` sets `day` even when the interval it charges is zero. So each case spawns the
//! loop, waits for the observable that first tick produces, and aborts.

use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use nestwatch::config::data_paths;
use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::rules::{Rules, run_rules_enforcer};
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
        Feed::new(),
        idle_waker(),
    ))
}

fn tally_path() -> std::path::PathBuf {
    data_paths().dir.join("usage_state.json")
}

fn day_in(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("day")?.as_str().map(str::to_string)
}

/// The whole point of `TickMode`, observed through the loop rather than argued from its source.
///
/// One test rather than two, because `NESTWATCH_DATA_DIR` is process-global and two `#[tokio::test]`
/// functions in one binary run concurrently by default — they would race over the same override.
/// Sequencing them here also makes the paused phase carry its own weight: it leaves the data
/// directory empty, so the measuring phase's "the file now exists" is unambiguous.
#[tokio::test]
async fn a_paused_tick_records_nothing_and_an_unconfigured_one_still_measures() {
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

    // ---- Paused: the tick must reach the loop and then record nothing at all ----------------
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
    // The heartbeat is stamped on entry to the tick, and everything a stood-down tick does after
    // that is in-memory. Nothing on that path can ever write the tally, so there is no race in
    // the direction this assertion cares about; the margin only guards a wildly descheduled task.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !tally.exists(),
        "a paused tick wrote {}. Pause is the control that means 'stop watching him' — it must \
         leave a gap in the record, not a quieter kind of surveillance.",
        tally.display()
    );
    paused.abort();

    // ---- Enabled with nothing configured: the tick must measure ----------------------------
    let measuring = spawn_enforcer(Rules::default()); // a fresh install, exactly as shipped
    let appeared = wait_for(|| tally.exists()).await;
    measuring.abort();

    assert!(
        appeared,
        "an enabled install with no rules yet wrote no tally at all. This is the defect \
         `TickMode::Measure` exists to prevent: it is the state every install starts in, the \
         dashboard calls it \"tracking only\", and `doctor` calls it \"counting screen time\"."
    );
    assert!(
        day_in(&tally).is_some(),
        "the tally exists but carries no `day`, so nothing was accrued into it — the loop \
         reached the write without reaching `decide`"
    );
}
