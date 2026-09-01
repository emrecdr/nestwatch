//! That a refusal counted anywhere in the process reaches the day the parent reads.
//!
//! # Why this drives the loop instead of testing the pieces
//!
//! `refusals` is three atomics and a `swap`; `Usage::refused` is a serde field; `today_summary`
//! reads it. Each half is easy to test and each half was, and **all of it passed with the one line
//! that connects them deleted.** That was measured, not feared: removing
//! `enforcer.usage.refused.merge(refusals::drain())` from the tick left the entire suite green —
//! 25 binaries, 585 tests — while the feature did nothing at all. Counters would have climbed
//! forever and the card would have read zero.
//!
//! That is the class `docs/OPEN-FINDINGS.md` `O75` names — pure helpers tested thoroughly, the
//! lines that call them not tested at all — and the same wiring gap `enforcer_app_stopped.rs`
//! exists for. So this asserts on the bytes that actually land in `usage_state.json`, produced by
//! the loop's own code.
//!
//! # Why its own binary
//!
//! Two reasons, and the second is specific to this file. `NESTWATCH_DATA_DIR` is process-global,
//! which is why `enforcer_loop.rs`, `enforcer_shutdown.rs` and `enforcer_app_stopped.rs` each have
//! one. On top of that the refusal counters are **also** process-global by design (see
//! `refusals`), so a second test in this binary incrementing them would be indistinguishable from
//! the enforcer draining them, and this test would pass or fail on scheduling.

use std::sync::{Arc, RwLock};

use nestwatch::config::Language;
use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::refusals;
use nestwatch::rules::{Rules, Usage, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, test_config, wait_for};

#[tokio::test]
async fn a_refusal_counted_anywhere_reaches_the_day_the_parent_reads() {
    let tmp = ScratchDir::new("enforcer-refusals");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let mut cfg = test_config();
    cfg.language = Language::En;
    cfg.rules = Rules {
        enabled: true,
        // A budget large enough that nothing is ever over it. This test is about the *reporting*
        // path, and an enforcement action firing here would be noise that could also mask the
        // thing under test by changing the tally for its own reasons.
        daily_budget_mins: 10_000,
        ..Default::default()
    };
    assert!(!cfg.curfew.enabled, "no curfew, so no shutdown interferes");

    // Counted *before* the loop starts, which is the honest shape: `clock::log_tamper` fires from
    // whatever thread happened to read the clock, and curfew increments from its own task. Neither
    // has any handle on the rules enforcer. The drain is what makes that work, so this test refuses
    // to hand the enforcer anything by another route.
    //
    // Three different counters, because a drain that moved one field and dropped the others would
    // otherwise pass — and a partial drain is a plausible edit, not a contrived one.
    refusals::clock_change_rejected();
    refusals::clock_change_rejected();
    refusals::day_reset_refused();
    refusals::shutdown_cancel_seen();
    refusals::shutdown_cancel_seen();
    refusals::shutdown_cancel_seen();

    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    let (_waker, wake) = tokio::sync::watch::channel(0u64);
    let loop_handle = tokio::spawn(run_rules_enforcer(
        control,
        Arc::new(RwLock::new(cfg)),
        Arc::new(UsageLog::new(tmp.join("usage.jsonl"))),
        Arc::new(ScreentimeLog::disabled()),
        Feed::new(),
        wake,
    ));

    // The tally is written by `save_tally_if_changed` once the first tick has folded the drain in.
    let tally = tmp.join("usage_state.json");
    let landed = wait_for(|| {
        std::fs::read_to_string(&tally)
            .ok()
            .and_then(|s| serde_json::from_str::<Usage>(&s).ok())
            .is_some_and(|u| u.refused.any())
    })
    .await;
    loop_handle.abort();

    assert!(
        landed,
        "nothing the process refused reached {}. The counters increment and the day never learns \
         of them, so the dashboard reports a quiet day while the clock was moved twice, a day \
         reset was blocked and three shutdowns were cancelled",
        tally.display()
    );

    // Read as bytes first, then parsed. The bytes are the part that matters: this is the property
    // that makes the counts worth storing at all, because a reboot is the cheapest thing a child
    // can do and an in-memory tally would be cleared by the very person it describes. The struct
    // being right in a live process proves nothing about what survives the power going off.
    let raw = std::fs::read_to_string(&tally).expect("tally");
    assert!(
        raw.contains("\"refused\""),
        "the counts are not in the persisted bytes, so a reboot erases them: {raw}"
    );
    let stored: Usage = serde_json::from_str(&raw).expect("the tally parses");

    // Each field separately: a drain that moved only the first would satisfy `any()` above.
    assert_eq!(stored.refused.clock_changes, 2, "clock changes");
    assert_eq!(stored.refused.day_resets, 1, "day resets");
    assert_eq!(stored.refused.shutdown_cancels, 3, "shutdown cancels");
    assert_eq!(stored.refused.total(), 6);
}
