//! That the loop which actually powers the PC off holds a gated child to the gate — and lets go of
//! him the moment this machine can no longer check.
//!
//! # Why this drives the loop instead of testing the pieces
//!
//! `Config::gate_cap_mins`, `probe::providers_not_checking` and `Rules::effective_budget_mins` each
//! have their own tests, and the day the dashboard and the child's page report is tested through
//! the real endpoints in `earned_grant.rs`. None of that reaches `run_rules_enforcer`, which is the
//! only one of them that enforces anything. **Measured, not feared:** with the enforcer's ceiling
//! replaced by `None` the whole suite passed, 699 tests — the gate did nothing to the child while
//! every card he and his parent could see said 35 minutes. Replacing only the enforcer's
//! cannot-check set with an empty one passed too, which would have turned a StudyGo outage into a
//! lost day with nothing red. That is `docs/OPEN-FINDINGS.md` `O75`'s class again: every piece
//! tested, the line that joins them not.
//!
//! So this asserts on what the loop's own code does and writes: the shutdown it asks for, and the
//! budget it records having enforced.
//!
//! # Why its own binary
//!
//! `NESTWATCH_DATA_DIR` is process-global, for the reason `enforcer_loop.rs` gives.

use std::sync::{Arc, RwLock};

use nestwatch::config::{Gate, Probe, Provider};
use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::probe::{ProbeOutcome, ProbeState, ProbeStatus, StatusMap};
use nestwatch::rules::{EnforceAction, Rules, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, test_config, wait_for};

/// The parent's own day, and the plugin's allowance inside it. Far enough apart, and far enough
/// from the hour already used, that which one the loop is enforcing is never a rounding question.
const PARENTS_DAY: u64 = 120;
const ALLOWANCE: u64 = 35;

/// The budget recorded with the first `event` line the loop wrote, if it has written one.
///
/// Read from the log rather than inferred from the shutdown alone, because the shutdown only says
/// the child was over *some* budget. The number says which one.
fn budget_logged(log: &std::path::Path, event: &str) -> Option<u64> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|line| line["event"] == event)
        .and_then(|line| line["budget"].as_u64())
}

#[tokio::test]
async fn a_gate_holds_the_child_in_the_loop_that_enforces_and_lets_go_when_it_cannot_check() {
    let tmp = ScratchDir::new("enforcer-gate");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    // An hour used: well over the allowance, well under his normal day.
    common::seed_tally(nestwatch::config::today(), 60 * 60);

    let mut cfg = test_config();
    cfg.rules = Rules {
        enabled: true,
        daily_budget_mins: PARENTS_DAY as u32,
        // Shutdown rather than Lock, because a shutdown is both observable on the fake and
        // cancellable — and the cancel is the half of this test that matters most.
        budget_action: EnforceAction::Shutdown,
        warn_secs: 45,
        ..Default::default()
    };
    cfg.providers.insert(
        "studygo".into(),
        Provider {
            enabled: true,
            minutes: 30,
            daily_cap_mins: None,
            tiers: Vec::new(),
            // A probe, because only a provider this machine checks itself can be reported as
            // unable to check. A push-only gate has no failure to observe.
            probe: Some(Probe {
                exe: "studygo-probe".into(),
                every_mins: 15,
                first_check_after_mins: 0,
            }),
            remind_every_check: false,
            gate: Some(Gate {
                allowance_mins: ALLOWANCE as u32,
                questions: 15,
                minutes_practised: 30,
            }),
        },
    );

    let status: StatusMap = Default::default();
    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    let log = tmp.path().join("usage.jsonl");
    // A wake this test holds, because the second half has to make the loop look again the way a
    // parent's change does — without waiting out a thirty-second tick.
    let (waker, wake) = tokio::sync::watch::channel(0u64);
    let loop_handle = tokio::spawn(run_rules_enforcer(
        control,
        Arc::new(RwLock::new(cfg)),
        Arc::new(UsageLog::new(log.clone())),
        Arc::new(ScreentimeLog::disabled()),
        // What the server hands it, over a status map this test can fail a check in.
        nestwatch::probe::gate_ceiling(status.clone()),
        Feed::new(),
        wake,
    ));

    // --- The check works, so the gate binds --------------------------------------------------
    //
    // Nothing has failed — no probe has run at all, which is not the same thing — so he starts
    // his day under the gate and an hour is far past it.
    let shut = wait_for(|| !fake.shutdowns().is_empty()).await;
    assert!(
        shut,
        "an hour into a day gated at {ALLOWANCE} minutes, the enforcer never asked for a \
         shutdown — the gate was on every card and did nothing to the child"
    );
    // Waited for, not read straight after the shutdown: the loop asks for the shutdown and writes
    // this line later in the same tick (`log_transition`), so reading at once raced it and failed
    // about one run in eight on a loaded machine.
    wait_for(|| budget_logged(&log, "budget_shutdown").is_some()).await;
    assert_eq!(
        budget_logged(&log, "budget_shutdown"),
        Some(ALLOWANCE),
        "the shutdown must be for the gate's allowance, not the parent's own {PARENTS_DAY}"
    );

    // --- Then the check fails, and the day is his again ---------------------------------------
    //
    // The shutdown is already counting down. A StudyGo outage is not evidence he has not
    // practised, so the gate must lift — and lifting it has to reach the shutdown in flight, or
    // the machine powers off anyway while the dashboard says two hours.
    status.lock().unwrap().insert(
        "studygo".into(),
        ProbeState {
            last: ProbeStatus {
                at: chrono::Local::now().fixed_offset(),
                reported: None,
                outcome: ProbeOutcome::Failed("StudyGo did not answer".into()),
            },
            reminded_on: None,
        },
    );
    waker.send_modify(|n| *n += 1);

    let rescued = wait_for(|| budget_logged(&log, "budget_shutdown_aborted").is_some()).await;
    loop_handle.abort();
    assert!(
        rescued,
        "the check failed and the enforcer kept the shutdown — a StudyGo outage cost the child his \
         day, which is the one outcome the household chose against"
    );
    assert_eq!(
        budget_logged(&log, "budget_shutdown_aborted"),
        Some(PARENTS_DAY),
        "and what he is handed back is exactly his normal day, not the allowance plus change"
    );
}
