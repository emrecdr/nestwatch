//! What the child is told, and what the parent's history records, when a **rule** closes an app.
//!
//! Its own test binary for the reason `enforcer_shutdown.rs` and `enforcer_loop.rs` each have one:
//! `NESTWATCH_DATA_DIR` is process-global, so two `#[tokio::test]` functions in one binary would
//! race over the override.
//!
//! # Why this drives the loop rather than testing the message function
//!
//! The defect this closes was **entirely wiring**, which is the class `docs/OPEN-FINDINGS.md` O70
//! names as this project's recurring failure. Every piece needed to tell a child why their game
//! shut was already present and correct — `notify_child`, `with_hint`, `ask_hint`, the translation
//! convention — and the blocklist / per-app-limit / group-pool paths simply did not call any of
//! it. `decide` pushed `Kill(pid)`, the loop killed the pid, and that was the whole interaction:
//!
//! * the child saw a window vanish, which reads as a crash rather than as a rule;
//! * nothing was written to the usage log either, so a parent could not answer "did my limit
//!   actually fire?" from the dashboard at all;
//! * meanwhile the *budget* path warned three times, waited out a grace period, and printed the
//!   address to ask at.
//!
//! A unit test on `app_stopped_message` proves the sentence is translated and says nothing about
//! whether anything ever calls it. That is the same gap `enforcer_shutdown.rs` was written for,
//! and it stayed open here because `FakeControl::notify_user` only wrote to `tracing` — so until
//! its `notifications` log existed, **nothing in this repository could observe a single thing this
//! product says to a child.**
//!
//! So this asserts on the bytes the child would really see, and on the row the parent would really
//! read, both produced by the loop's own code.

use std::sync::{Arc, RwLock};

use nestwatch::config::Language;
use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::rules::{Rules, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, test_config, wait_for};

/// Deliberately not 8443, so a hint that hard-coded the default fails here instead of passing by
/// coincidence — the same trap `enforcer_shutdown.rs` sets.
const PORT: u16 = 9443;

/// `FakeControl::new` ships a fixed process list; this is the one the blocklist below names.
/// Taken from the fake rather than written out, so a change to that list breaks this loudly
/// instead of leaving the test waiting on a process that is no longer there.
const TARGET: &str = "Minecraft.exe";

#[tokio::test]
async fn a_dutch_child_is_told_which_app_a_rule_closed_and_where_to_ask() {
    let tmp = ScratchDir::new("enforcer-app-stopped");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let mut cfg = test_config();
    cfg.port = PORT;
    cfg.language = Language::Nl;
    cfg.rules = Rules {
        enabled: true,
        // No budget at all. That is the point: this path must work on an install that has only a
        // blocklist, which is also the install where the child has never seen a countdown and has
        // the least context for a window closing by itself.
        daily_budget_mins: 0,
        blocklist: vec![TARGET.to_string()],
        ..Default::default()
    };
    // Curfew off, so `ask_hint` is live — during a window it returns `None` on purpose, because
    // extra time cannot move bedtime.
    assert!(!cfg.curfew.enabled, "default curfew must be off here");

    let usage_log = Arc::new(UsageLog::new(tmp.join("usage.jsonl")));
    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    // A waker this test can pulse, rather than `common::idle_waker`.
    //
    // The loop ticks every 30 seconds and `tokio::time::pause()` is unusable here (see
    // `enforcer_loop.rs`: the loop measures elapsed time with `std::time::Instant`, which the
    // paused clock does not virtualise), so a driver test otherwise observes exactly **one** tick.
    // `heartbeat::tick` selects on this channel so a parent's change is seen before the next
    // interval; pulsing it drives further ticks in milliseconds.
    //
    // **What the extra ticks do and do not prove, measured rather than assumed.** They do NOT
    // cover the relaunch case: `FakeControl::kill_process` removes the process from its list, so
    // by tick two there is nothing left to kill, and a mutation that announced on every tick was
    // confirmed to pass this test even with them. What they do cover is the plausible wrong
    // shape — announcing from "this app is over its limit" rather than from "this app was killed
    // just now", which would keep emitting rows for an app that is already gone.
    //
    // The once-per-day rule itself is pinned where the process list is under the test's control,
    // in `rules.rs`: `app_limit_kills_when_exceeded` (third tick), `a_new_day_earns_a_fresh_
    // explanation`, and `group_pool_kills_all_members_when_spent`. All three were watched to fail
    // against that mutation.
    let (waker, wake) = tokio::sync::watch::channel(0u64);
    let loop_handle = tokio::spawn(run_rules_enforcer(
        control,
        Arc::new(RwLock::new(cfg)),
        usage_log.clone(),
        Arc::new(ScreentimeLog::disabled()),
        Feed::new(),
        wake,
    ));

    let arrived = wait_for(|| !fake.notification_bodies().is_empty()).await;

    // Two more ticks, on the same day, with the app still running and still blocked.
    for _ in 0..2 {
        nestwatch::heartbeat::wake(&waker);
        // Long enough for the woken tick to run to completion — it is pure decision work plus a
        // `spawn_blocking` kill against an in-memory list, so this is generous by orders of
        // magnitude rather than a race being papered over.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    loop_handle.abort();
    assert!(
        arrived,
        "a blocklisted app was killed and the child was told nothing. That is the whole defect: \
         the window vanishes and reads as a crash, so the rule shapes no habit and the child has \
         no idea they could ask about it"
    );

    let body = fake.notification_bodies()[0].clone();

    assert!(
        body.contains(TARGET),
        "the notice must name the app, or it explains nothing about the window that just \
         disappeared: {body}"
    );
    assert!(
        body.contains("geblokkeerd"),
        "a Dutch install must explain the closure in Dutch — this is the first message this path \
         has ever sent, so there is no older behaviour to inherit correctness from: {body}"
    );
    assert!(
        !body.contains("blocked"),
        "English leaked into the Dutch notice: {body}"
    );
    assert!(
        body.contains(&format!("https://localhost:{PORT}/ask")),
        "the notice must carry the address to ask at, on the port this install actually uses. \
         Every other child-facing message in this crate does, and the one that did not was a \
         shipped defect: {body}"
    );

    // The parent's half. Without this row the dashboard cannot distinguish "the blocklist fired"
    // from "the child never opened it", which are the same picture: an app with no minutes.
    let rows = usage_log.recent(20);
    let stopped: Vec<_> = rows
        .iter()
        .filter(|r| r["event"] == "app_stopped")
        .collect();
    assert_eq!(
        stopped.len(),
        1,
        "exactly one `app_stopped` row, across three ticks of which only the first had anything \
         to kill — none means the parent cannot verify their own rule fired, and more than one \
         means the row is driven by 'still over the limit' rather than by 'killed just now', so \
         an app already closed keeps writing history the parent has to read past: {rows:#?}"
    );
    assert_eq!(
        fake.notification_bodies().len(),
        1,
        "and exactly one dialog, for the same reason — a notice tied to the limit rather than to \
         the kill would keep firing at a child whose app is already gone: {:?}",
        fake.notification_bodies()
    );
    let row = stopped[0];
    assert_eq!(row["app"], TARGET);
    assert_eq!(
        row["reason"], "blocklist",
        "the reason is a stable tag, not a Debug rendering that renaming a variant would change"
    );
    assert_eq!(
        row["limit_mins"],
        serde_json::Value::Null,
        "the blocklist has no time in it, and `null` is not `0` — a zero limit is a real and \
         different thing (an app switched off)"
    );
    assert_eq!(
        row["notified"], true,
        "the row records whether the child actually saw it, so a parent can tell an enforced rule \
         from an explained one"
    );
}
