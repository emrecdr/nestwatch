//! A scripted afternoon through the loop that enforces screen time: the three warnings in order,
//! each said once, then the shutdown, then a parent's grant calling it off and re-arming the
//! warnings — asserted on the control calls the child would actually see, in the order they
//! happened.
//!
//! # Why a sequence, when every piece has a test
//!
//! `countdown.rs` pins *when to speak* in isolation, `RulesEnforcer::decide` is driven over
//! multi-tick sequences in seventeen unit tests, and `enforcer_shutdown.rs` runs one budget out to
//! one shutdown. What none of them sees is the **order of what reaches the child**: a warning that
//! fires twice, one that fires after the ending, a grant that does not re-arm, or an audit row
//! recorded for a notice that was never delivered. `docs/OPEN-FINDINGS.md` `O70` names that shape —
//! ordering is where this project's wiring defects have lived — and the gate work this month found
//! two more of them (`ad21b41`, `bcb4a7f`) at exactly the join between tested pieces.
//!
//! # How it drives a 30-second loop in seconds
//!
//! Not with `tokio::time::pause` — the loop charges `std::time::Instant` elapsed time, which a
//! paused clock does not move (see `enforcer_loop.rs`). The budget is walked instead: the tally is
//! seeded at a hundred minutes and the **daily limit is lowered** step by step through the config
//! the loop reads on every tick, with a wake after each write the way a parent's edit wakes it.
//! Remaining time then crosses 15, 5, 1 and 0 in turn, which is the afternoon as the enforcer
//! experiences it, in four ticks instead of twenty minutes.
//!
//! # Why its own binary
//!
//! `NESTWATCH_DATA_DIR` is process-global, for the reason `enforcer_loop.rs` gives.

use std::sync::{Arc, RwLock};

use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::rules::{EnforceAction, Rules, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, test_config, wait_for};

/// An hour and forty minutes used, so a limit of 120 leaves twenty: above every threshold.
const USED_MINS: u64 = 100;

/// The `event` rows the loop wrote, oldest first.
fn events(log: &std::path::Path, event: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|line| line["event"] == event)
        .collect()
}

#[tokio::test]
async fn an_afternoon_warns_three_times_in_order_shuts_down_once_and_re_arms_after_a_grant() {
    let tmp = ScratchDir::new("enforcer-day");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };
    let today = nestwatch::config::today();
    common::seed_tally(today, USED_MINS * 60);

    let mut cfg = test_config();
    cfg.rules = Rules {
        enabled: true,
        daily_budget_mins: 120,
        budget_action: EnforceAction::Shutdown,
        warn_secs: 45,
        ..Default::default()
    };
    let config = Arc::new(RwLock::new(cfg));
    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    let log = tmp.path().join("usage.jsonl");
    let (waker, wake) = tokio::sync::watch::channel(0u64);
    let loop_handle = tokio::spawn(run_rules_enforcer(
        control,
        config.clone(),
        Arc::new(UsageLog::new(log.clone())),
        Arc::new(ScreentimeLog::disabled()),
        nestwatch::probe::gate_ceiling(Default::default()),
        Feed::new(),
        wake,
    ));
    let heard = || fake.notification_bodies();
    // A parent's edit: write the config the loop reads, then wake it the way `try_update_config`
    // does, and wait for the one thing the step should produce.
    let set_limit = |mins: u32| {
        config.write().unwrap().rules.daily_budget_mins = mins;
        waker.send_modify(|n| *n += 1);
    };

    // --- Twenty minutes left: the first tick primes the countdown and says nothing -------------
    assert!(
        wait_for(|| nestwatch::heartbeat::worst_age_secs().is_some()).await,
        "the loop never ticked"
    );
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        heard(),
        Vec::<String>::new(),
        "nothing is said above the first threshold"
    );

    // --- 14 left: the fifteen-minute warning, once --------------------------------------------
    set_limit(114);
    assert!(
        wait_for(|| heard().len() == 1).await,
        "no warning at 14 minutes: {:?}",
        heard()
    );
    assert!(
        heard()[0].starts_with("15 minutes of screen time left"),
        "the threshold is announced, not the reading: {}",
        heard()[0]
    );

    // --- 4 left: the five-minute warning, and the fifteen is not repeated ----------------------
    set_limit(104);
    assert!(
        wait_for(|| heard().len() == 2).await,
        "no warning at 4 minutes: {:?}",
        heard()
    );
    assert!(
        heard()[1].starts_with("5 minutes of screen time left"),
        "{}",
        heard()[1]
    );

    // --- 1 left: the last warning ------------------------------------------------------------
    set_limit(101);
    assert!(
        wait_for(|| heard().len() == 3).await,
        "no warning at 1 minute: {:?}",
        heard()
    );
    assert!(
        heard()[2].starts_with("1 minute of screen time left"),
        "{}",
        heard()[2]
    );
    assert_eq!(
        events(&log, "budget_countdown")
            .iter()
            .map(|row| row["minutes_remaining"].as_u64().unwrap_or(0))
            .collect::<Vec<_>>(),
        vec![15, 5, 1],
        "the history records each delivered warning once, in the order it was said"
    );

    // --- 0 left: the shutdown, asked for once; Windows' own box carries its message --------
    set_limit(100);
    assert!(
        wait_for(|| !fake.shutdowns().is_empty()).await,
        "the budget ran out and nothing happened"
    );
    assert_eq!(fake.shutdowns().len(), 1, "one shutdown, not one per tick");
    assert_eq!(
        fake.shutdowns()[0].0,
        45,
        "the grace period is the configured one"
    );
    assert!(
        fake.shutdowns()[0].1.is_some(),
        "and the box the child sees says why"
    );
    assert!(
        wait_for(|| !events(&log, "budget_shutdown").is_empty()).await,
        "the shutdown is recorded"
    );
    assert_eq!(
        events(&log, "budget_shutdown")[0]["budget"],
        serde_json::json!(100)
    );
    assert_eq!(
        heard().len(),
        3,
        "no fourth notice: Windows' shutdown box already carries the message, so it is not \
         doubled up — and nothing else was said: {:?}",
        heard()
    );

    // --- A parent grants half an hour: the shutdown is called off and nothing more is said ---
    config.write().unwrap().extra.add(today, 30);
    waker.send_modify(|n| *n += 1);
    assert!(
        wait_for(|| !events(&log, "budget_shutdown_aborted").is_empty()).await,
        "a grant that lifts him back under budget must call the shutdown off"
    );
    assert_eq!(
        events(&log, "budget_shutdown_aborted")[0]["budget"],
        serde_json::json!(130),
        "called off at the day the grant made"
    );
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        heard().len(),
        3,
        "a grant is good news and is not announced as a warning"
    );

    // --- The grant re-armed the countdown: crossing fifteen again is said again ---------------
    //
    // 84 + the 30 granted is 114, so fourteen minutes are left once more. Said again because the
    // child's situation is new; a countdown that stayed spent after a grant would let the second
    // half of the afternoon end with no warning at all.
    //
    // **Two spellings re-arm it, and this assertion needs both removed to fail.** Measured: with
    // the spent-budget `countdown.reset()` in `RulesEnforcer::decide` deleted, this passes
    // (`Countdown::observe` re-arms on any rising reading); with `observe`'s memory pinned so it
    // can only fall, this passes (the reset re-primes it); with both gone, this is the line that
    // fails. Defence in depth rather than dead code — the shape `control/mod.rs` documents for the
    // probe timeout — so do not delete either on the strength of a single surviving mutant.
    set_limit(84);
    assert!(
        wait_for(|| heard().len() == 4).await,
        "no warning after the grant: {:?}",
        heard()
    );
    assert!(
        heard()[3].starts_with("15 minutes of screen time left"),
        "{}",
        heard()[3]
    );
    loop_handle.abort();

    assert_eq!(
        fake.shutdowns().len(),
        1,
        "and the shutdown that was called off is not re-issued by the warnings"
    );
}
