//! A routine that will start below what today has used is counted down to through the real loop.
//!
//! `rules.rs`'s unit tests hand `decide` the budgets ahead directly. This drives the wiring they
//! cannot see: that `run_rules_enforcer` reads the schedule ahead of the trusted clock and passes it
//! on, so the 15/5/1 warnings reach the child before the routine locks the PC, as the README
//! promises. Before this, a "Homework, 16:00-18:00" routine with a 60-minute limit, on a day with 75
//! minutes used, locked at 16:00 with a minute's notice.
//!
//! The routine is moved rather than the clock: each step writes a schedule starting a given number
//! of minutes from now and wakes the loop, the way a parent's edit does.
//!
//! Its own binary: `NESTWATCH_DATA_DIR` is process-global.

use std::sync::{Arc, RwLock};

use nestwatch::config::Routine;
use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::curfew::Window;
use nestwatch::foreground::Feed;
use nestwatch::rules::{Rules, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, seed_tally, test_config, wait_for};

/// A 60-minute "Homework" routine whose window opens `mins` minutes from now and lasts an hour.
fn homework_in(mins: i64) -> Routine {
    let start = nestwatch::clock::now() + chrono::Duration::minutes(mins);
    let hm = |t: chrono::DateTime<chrono::FixedOffset>| t.format("%H:%M").to_string();
    Routine {
        name: "Homework".into(),
        rules: Rules {
            enabled: true,
            daily_budget_mins: 60,
            ..Default::default()
        },
        schedule: vec![Window {
            start: hm(start),
            end: hm(start + chrono::Duration::hours(1)),
            days: Default::default(),
        }],
    }
}

/// How far ahead the routine first appears: past the 15-minute warning, so seeing it only primes
/// the countdown.
const FIRST_SIGHTING_MINS: i64 = 16;

#[tokio::test]
async fn a_routine_that_will_start_below_todays_use_is_counted_down_to() {
    // The routine must start *today*: one past midnight begins after the day's use resets, so it
    // has nothing to count down to, and the enforcer is right to stay quiet about it.
    common::clear_of_midnight_by(
        std::time::Duration::from_secs(FIRST_SIGHTING_MINS as u64 * 60) + common::DAY_LEFT_NEEDED,
    );
    let tmp = ScratchDir::new("enforcer-routine");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };
    seed_tally(nestwatch::config::today(), 75 * 60);

    // The day's own rules set no limit; only the routine does.
    let mut cfg = test_config();
    cfg.rules = Rules {
        enabled: true,
        daily_budget_mins: 0,
        ..Default::default()
    };
    cfg.routines = vec![homework_in(FIRST_SIGHTING_MINS)];
    let config = Arc::new(RwLock::new(cfg));

    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    let (waker, wake) = tokio::sync::watch::channel(0u64);
    let loop_handle = tokio::spawn(run_rules_enforcer(
        control,
        config.clone(),
        Arc::new(UsageLog::new(tmp.join("usage.jsonl"))),
        Arc::new(ScreentimeLog::new(tmp.join("screentime.jsonl"))),
        nestwatch::probe::gate_ceiling(Default::default()),
        Feed::new(),
        wake,
    ));
    let heard = || fake.notification_bodies();
    let starts_in = |mins: i64| {
        config.write().unwrap().routines = vec![homework_in(mins)];
        waker.send_modify(|n| *n += 1);
    };

    // Sixteen minutes out: the first sighting, which only primes the countdown.
    assert!(
        wait_for(|| nestwatch::heartbeat::worst_age_secs().is_some()).await,
        "the loop never ticked"
    );
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(heard().is_empty(), "{:?}", heard());

    for (mins, said) in [
        (14, "15 minutes of screen time left today."),
        (4, "5 minutes of screen time left — good time to save."),
    ] {
        let before = heard().len();
        starts_in(mins);
        assert!(
            wait_for(|| heard().len() > before).await,
            "nothing said with the routine {mins} minutes away: {:?}",
            heard()
        );
        let last = heard().last().cloned().unwrap_or_default();
        assert!(last.starts_with(said), "{mins} minutes away: {last}");
    }
    loop_handle.abort();
    assert!(
        fake.shutdowns().is_empty(),
        "nothing is enforced before the routine starts"
    );
}
