//! A limit set after the play is a limit on the day, through the real loop.
//!
//! A parent saw 200 minutes of Roblox and set a 45-minute limit on "Roblox", and the game kept
//! running: the name matched no program, and a limit counted only from when it was saved. The
//! unit tests in `rules.rs` hand `decide` a running count and an owner snapshot directly. This
//! drives the wiring they cannot see: that `run_rules_enforcer` reads the day's running count back
//! from the tally, asks Windows who owns each process for a `*` pattern, starts the new limit from
//! what its programs already ran, and closes them at the first check.
//!
//! Minecraft stands in for Roblox because it is already in the fake's process list.
//!
//! Its own binary: `NESTWATCH_DATA_DIR` is process-global, so the two scenarios run in turn from
//! one test.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::rules::{Rules, Usage, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{FAKE_MINECRAFT, ScratchDir, test_config, wait_for};

#[tokio::test]
async fn enforcer_names() {
    common::clear_of_midnight();
    let tmp = ScratchDir::new("enforcer-names");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };
    a_limit_with_stars_set_after_the_play_closes_the_game_at_the_first_check(&tmp).await;
    std::fs::remove_file(tmp.join("usage_state.json")).unwrap();
    the_days_running_count_grows_with_no_rules_set(&tmp).await;
}

/// The loop under `rules`, started on the fake, until the returned handle is aborted.
fn start(
    tmp: &ScratchDir,
    rules: Rules,
) -> (
    Arc<FakeControl>,
    tokio::sync::watch::Sender<u64>,
    tokio::task::JoinHandle<()>,
) {
    let mut cfg = test_config();
    cfg.rules = rules;
    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    let (waker, wake) = tokio::sync::watch::channel(0u64);
    let handle = tokio::spawn(run_rules_enforcer(
        control,
        Arc::new(RwLock::new(cfg)),
        Arc::new(UsageLog::new(tmp.join("usage.jsonl"))),
        Arc::new(ScreentimeLog::new(tmp.join("screentime.jsonl"))),
        nestwatch::probe::gate_ceiling(Default::default()),
        Feed::new(),
        wake,
    ));
    (fake, waker, handle)
}

async fn a_limit_with_stars_set_after_the_play_closes_the_game_at_the_first_check(
    tmp: &ScratchDir,
) {
    // 249 minutes used today, 200 of them in Minecraft, before any limit existed.
    let played = Usage {
        day: Some(nestwatch::config::today()),
        total_secs: 249 * 60,
        running_secs: BTreeMap::from([("minecraft.exe".to_string(), 200 * 60)]),
        ..Default::default()
    };
    std::fs::create_dir_all(tmp.path()).unwrap();
    std::fs::write(
        tmp.join("usage_state.json"),
        serde_json::to_string(&played).unwrap(),
    )
    .unwrap();

    let (fake, _waker, loop_handle) = start(
        tmp,
        Rules {
            enabled: true,
            app_limits: [("*CRAFT*".to_string(), 45)].into(),
            ..Default::default()
        },
    );

    let running = || {
        fake.list_processes()
            .unwrap()
            .iter()
            .any(|p| p.pid == FAKE_MINECRAFT)
    };
    let closed = wait_for(|| !running()).await;
    loop_handle.abort();
    assert!(
        closed,
        "Minecraft is still running under a 45-minute limit after 200 minutes"
    );
    assert!(
        fake.list_processes()
            .unwrap()
            .iter()
            .any(|p| p.name == "chrome.exe"),
        "only what the pattern names is closed"
    );
}

/// With no rule set at all, the loop still counts what the child runs, so the first limit a parent
/// sets later has a day to start from. Two ticks a second apart, since a tick counts the whole
/// seconds that passed.
async fn the_days_running_count_grows_with_no_rules_set(tmp: &ScratchDir) {
    let (_fake, waker, loop_handle) = start(
        tmp,
        Rules {
            enabled: true,
            ..Default::default()
        },
    );
    let tally = || {
        std::fs::read_to_string(tmp.join("usage_state.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Usage>(&t).ok())
    };
    assert!(
        wait_for(|| tally().is_some()).await,
        "the loop never ticked"
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    waker.send_modify(|n| *n += 1);
    let counted = wait_for(|| {
        tally().is_some_and(|u| u.running_secs.get("minecraft.exe").is_some_and(|s| *s >= 1))
    })
    .await;
    loop_handle.abort();
    assert!(counted, "no running count: {:?}", tally());
}
