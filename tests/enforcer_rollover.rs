//! The morning after a day that ended over its limit, with the rules on: the first tick turns the
//! day, writes yesterday's row, and judges today by today's minutes.
//!
//! `enforcer_paused.rs` crosses midnight with the rules paused; this is the ordinary case, a loop
//! that measures across the date change or is started the next morning. A tally carried onto the
//! new day would spend the morning against yesterday's 95 minutes and lock the child out on the
//! first tick, and a countdown counting yesterday's minutes would warn about a limit already spent.
//! Nothing drove the measuring rollover through the loop before this.
//!
//! The loop's `today` is not injectable, but its tally is read from disk, so seeding one dated
//! yesterday puts the loop on the far side of midnight from its first tick.
//!
//! Its own binary: `NESTWATCH_DATA_DIR` is process-global.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::rules::{Rules, Usage, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, seed_tally, test_config, wait_for};

const YESTERDAY_MINS: u64 = 95;
const LIMIT_MINS: u32 = 60;

#[tokio::test]
async fn the_morning_after_an_over_limit_day_is_judged_by_its_own_minutes() {
    let tmp = ScratchDir::new("enforcer-rollover");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let today = nestwatch::config::today();
    let yesterday = today.pred_opt().expect("a representable yesterday");
    seed_tally(yesterday, YESTERDAY_MINS * 60);

    let mut cfg = test_config();
    cfg.rules = Rules {
        enabled: true,
        daily_budget_mins: LIMIT_MINS,
        ..Default::default()
    };
    let config = Arc::new(RwLock::new(cfg));

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

    // The first tick turns the day.
    assert!(
        wait_for(|| stored().is_some_and(|u| u.day == Some(today))).await,
        "the tally never moved onto today: {:?}",
        stored()
    );
    let turned = stored().expect("the tally");
    assert!(
        turned.total_secs < 60,
        "yesterday's minutes were carried into today: {turned:?}"
    );
    assert_eq!(
        turned.refused.day_resets, 0,
        "the date change is an honoured reset"
    );

    // Yesterday's row says what it measured. It names no limit: a loop that has just started does
    // not know the one yesterday ended with, and claims none rather than today's, which once wrote
    // "over budget" on days whose limit was never today's (see `rollup_row`).
    let yesterday_row = || {
        std::fs::read_to_string(&screentime)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|r| r["date"] == yesterday.to_string())
    };
    // Waited for rather than read once: the row lands a moment after the tally does.
    assert!(
        wait_for(|| yesterday_row().is_some()).await,
        "no row for {yesterday}"
    );
    let row = yesterday_row().expect("just seen");
    assert_eq!(row["minutes_used"], YESTERDAY_MINS, "{row}");
    assert!(row.get("budget").is_none(), "{row}");

    // Long enough for a lock warning or a shutdown to be sent, had either been decided.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        fake.shutdowns().is_empty(),
        "shut down over yesterday's minutes"
    );
    assert_eq!(
        fake.notification_bodies(),
        Vec::<String>::new(),
        "warned the child about yesterday's limit"
    );

    // Today's countdown counts today's minutes: a limit lowered to fourteen leaves fourteen, and
    // the child hears the fifteen-minute warning. Counting yesterday's would leave nothing.
    config.write().unwrap().rules.daily_budget_mins = 14;
    waker.send_modify(|n| *n += 1);
    assert!(
        wait_for(|| !fake.notification_bodies().is_empty()).await,
        "no warning once today's limit was lowered"
    );
    loop_handle.abort();
    let heard = fake.notification_bodies();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(
        heard[0].starts_with("15 minutes of screen time left today."),
        "{heard:?}"
    );
    assert!(fake.shutdowns().is_empty());
}
