//! A pause that runs past midnight: the day still turns, and what is refused still reaches the card.
//!
//! # What broke
//!
//! `accounting_day` runs on every tick, paused or not, and records a day change as an honoured
//! reset. Only `decide` moved the tally onto the new day, and `decide` does not run while the rules
//! are paused. So the first paused tick after midnight booked the reset and left the tally on
//! yesterday, and every paused tick after it looked like a *second* reset inside twelve hours:
//!
//! - each was refused and counted, about 120 an hour for as long as the pause lasted (959 in eight
//!   hours, measured), all landing on the card the day the rules came back on;
//! - the first ticks after the pause were accounted against the paused day, so for up to twelve
//!   hours the new day spent the old day's limit, and a day that ended over it locked the next
//!   morning on its first tick;
//! - and nothing refused while paused reached the card at all, because the paused branch neither
//!   drained the counters nor saved the tally.
//!
//! # How this crosses midnight without a clock
//!
//! The loop's `today` is not injectable (`docs/OPEN-FINDINGS.md` `O70`), but its tally is read
//! from disk. A tally dated yesterday is exactly what a pause that ran past midnight leaves behind,
//! so seeding one puts the loop on the far side of midnight from its first tick.
//!
//! # Why its own binary
//!
//! `NESTWATCH_DATA_DIR` is process-global, and so are the refusal counters this test counts with,
//! for the reasons `enforcer_refusals.rs` gives.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::refusals;
use nestwatch::rules::{Rules, Usage, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, seed_tally, test_config, wait_for};

/// Minutes yesterday used, against a limit of [`LIMIT_MINS`]: a day that ended over its limit, so
/// accounting the morning against it would lock at once.
const YESTERDAY_MINS: u64 = 95;
const LIMIT_MINS: u32 = 60;

#[tokio::test]
async fn a_pause_across_midnight_still_turns_the_day_and_keeps_what_was_refused() {
    let tmp = ScratchDir::new("enforcer-paused");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let today = nestwatch::config::today();
    let yesterday = today.pred_opt().expect("a representable yesterday");
    seed_tally(yesterday, YESTERDAY_MINS * 60);

    let mut cfg = test_config();
    cfg.rules = Rules {
        enabled: false, // paused: the free evening, still on after midnight
        daily_budget_mins: LIMIT_MINS,
        ..Default::default()
    };
    assert!(
        !cfg.curfew.enabled,
        "no curfew, so nothing else speaks to the child"
    );
    let config = Arc::new(RwLock::new(cfg));

    // Two wrong passwords before the first tick, while paused.
    refusals::wrong_password_refused();
    refusals::wrong_password_refused();

    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    let screentime = tmp.join("screentime.jsonl");
    let (waker, wake) = tokio::sync::watch::channel(0u64);
    let loop_handle = tokio::spawn(run_rules_enforcer(
        control,
        config.clone(),
        Arc::new(UsageLog::new(tmp.join("usage.jsonl"))),
        Arc::new(ScreentimeLog::new(screentime.clone())),
        nestwatch::probe::gate_ceiling(Default::default()),
        Feed::new(),
        wake,
    ));

    let tally = tmp.join("usage_state.json");
    let stored = || {
        std::fs::read_to_string(&tally)
            .ok()
            .and_then(|s| serde_json::from_str::<Usage>(&s).ok())
    };
    let wrong_passwords_today = || {
        stored()
            .filter(|u| u.day == Some(today))
            .map(|u| u.refused.wrong_passwords)
    };

    assert!(
        wait_for(|| wrong_passwords_today() == Some(2)).await,
        "while paused, the tally never moved onto today with the two wrong passwords on it: \
         {:?}. The paused branch has to turn the day and keep what was refused, or every \
         paused tick after midnight is refused as a second day reset and the passwords wait \
         for whichever day the rules resume on",
        stored()
    );

    // More paused ticks, each woken with one more wrong password, so the count proves the tick
    // ran: a tick that changes nothing writes nothing, and could not be told from no tick at all.
    for expected in 3..=6 {
        refusals::wrong_password_refused();
        waker.send_modify(|n| *n += 1);
        assert!(
            wait_for(|| wrong_passwords_today() == Some(expected)).await,
            "paused tick {expected} did not keep its wrong password: {:?}",
            stored()
        );
    }
    let paused = stored().expect("the tally");
    assert_eq!(
        paused.refused.day_resets, 0,
        "paused ticks refused day resets that nobody attempted: once the day has turned, a \
         tick on the same day is not a reset"
    );
    assert_eq!(paused.total_secs, 0, "nothing is measured while paused");

    // Yesterday ended when midnight passed, paused or not, and its row says what it measured.
    let rows = std::fs::read_to_string(&screentime).unwrap_or_default();
    let yesterday_row = rows
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|r| r["date"] == yesterday.to_string());
    let yesterday_row = yesterday_row.unwrap_or_else(|| {
        panic!("no row for {yesterday}, the day the pause ran out of: {rows:?}")
    });
    assert_eq!(
        yesterday_row["minutes_used"], YESTERDAY_MINS,
        "{yesterday_row}"
    );

    // The parent switches the rules back on. Today has used nothing, so the first tick must not
    // judge it by yesterday's 95 minutes against a 60-minute limit. A little over a second first,
    // because a tick charges whole seconds since the last one and a prompt wake would charge none,
    // leaving "charged to today" unobservable.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    config.write().unwrap().rules.enabled = true;
    waker.send_modify(|n| *n += 1);
    assert!(
        wait_for(|| stored().is_some_and(|u| u.day == Some(today) && u.total_secs > 0)).await,
        "the tick after resuming charged nothing to today: {:?}",
        stored()
    );
    // Long enough for a lock warning to be sent, had one been decided.
    tokio::time::sleep(Duration::from_millis(150)).await;
    loop_handle.abort();

    assert_eq!(
        fake.notification_bodies(),
        Vec::<String>::new(),
        "resuming after midnight warned the child about yesterday's limit"
    );
    let resumed = stored().expect("the tally");
    assert_eq!(
        resumed.refused.wrong_passwords, 6,
        "resuming lost today's refusals"
    );
    assert!(
        !resumed.unmeasured,
        "today has been measured since the rules came back on, so its row must say what it measured"
    );
}
