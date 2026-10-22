//! Words on an app group, through the loop: a matching program and a matching page are closed once
//! the group's pool is spent, and only the child's own processes are.
//!
//! Its own test binary for the reason `enforcer_app_stopped.rs` gives: `NESTWATCH_DATA_DIR` is
//! process-global, so two `#[tokio::test]` functions in one binary would race over the override.
//!
//! # Why the loop and not only `decide`
//!
//! `decide` is pure and its tests in `rules.rs` cover the rules. What they cannot see is the wiring
//! this feature added around it: the loop has to clamp the helper's report *before* `decide` rather
//! than only inside `record_foreground`, and it has to ask `SystemControl::child_processes` — and pass
//! the answer through — on ticks where a group has words. A loop that forgot either would leave
//! every unit test green and close nothing, which is this project's recurring failure (`O70`).
//!
//! # Why one second of real waiting
//!
//! The loop charges real elapsed time and `clamp` caps a report's seconds at it, so on the
//! immediate first tick a page visit is clamped to nothing. The program half needs no time at all —
//! a spent pool closes a matching program on sight — but the page half needs a tick that is at
//! least one whole second after the last, so the visit survives the clamp.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::{Feed, Sample};
use nestwatch::rules::{AppGroup, Rules, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{
    FAKE_CHROME as CHROME, FAKE_DISCORD as DISCORD, FAKE_MINECRAFT as MINECRAFT,
    FAKE_NOTEPAD as NOTEPAD, ScratchDir, test_config, wait_for,
};

fn running(fake: &FakeControl) -> BTreeSet<u32> {
    fake.list_processes()
        .expect("the fake lists")
        .into_iter()
        .map(|p| p.pid)
        .collect()
}

#[tokio::test]
async fn a_spent_pool_closes_the_childs_matching_program_and_browser() {
    common::clear_of_midnight();
    let tmp = ScratchDir::new("enforcer-words");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    // The pool is already spent when the loop starts.
    common::seed_usage(&nestwatch::rules::Usage {
        day: Some(nestwatch::config::today()),
        per_group_secs: [("Games".to_string(), 3_600)].into(),
        ..Default::default()
    });

    let mut cfg = test_config();
    cfg.rules = Rules {
        enabled: true,
        daily_budget_mins: 0,
        app_groups: vec![AppGroup {
            name: "Games".into(),
            apps: Vec::new(),
            // `craft` matches Minecraft.exe; `note` matches notepad.exe, which is not the child's;
            // `cord` matches Discord.exe, which is configured as a check program; `roblox` is the
            // page.
            words: vec![
                "craft".into(),
                "note".into(),
                "cord".into(),
                "roblox".into(),
            ],
            limit_mins: 60,
        }],
        ..Default::default()
    };
    common::with_check_program(&mut cfg, "Discord.exe");

    let fake = Arc::new(FakeControl::new());
    // Chrome, Minecraft and Discord are the child's. Notepad matches a word and is not, so it must
    // survive: that is the owner rule seen through the loop, not only through `decide`.
    fake.script_child_pids(Ok(BTreeSet::from([CHROME, MINECRAFT, DISCORD])));
    let control: Arc<dyn SystemControl> = fake.clone();
    let feed = Feed::new();
    let (waker, wake) = tokio::sync::watch::channel(0u64);
    let loop_handle = tokio::spawn(run_rules_enforcer(
        control,
        Arc::new(RwLock::new(cfg)),
        Arc::new(UsageLog::disabled()),
        Arc::new(ScreentimeLog::disabled()),
        nestwatch::probe::gate_ceiling(Default::default()),
        feed.clone(),
        wake,
    ));

    let program_closed = wait_for(|| !running(&fake).contains(&MINECRAFT)).await;

    // A matching page in Chrome, reported by the helper, then a tick more than a second later.
    let secret = "roblox.com/users/123/private-profile";
    feed.submit(Sample {
        web: [(format!("chrome.exe\nRoblox\n{secret}"), 30)].into(),
        ..Sample::default()
    });
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    nestwatch::heartbeat::wake(&waker);
    let browser_closed = wait_for(|| !running(&fake).contains(&CHROME)).await;
    loop_handle.abort();

    assert!(
        program_closed,
        "the pool was spent and Minecraft.exe matches `craft` under the child's account, but it \
         was not closed: the loop is not passing child_processes, or not matching words at all"
    );
    assert!(
        browser_closed,
        "Chrome reported a page titled Roblox while the pool was spent and was not closed: the \
         loop is not passing the helper's visits to decide, or clamped them away"
    );
    assert!(
        running(&fake).contains(&NOTEPAD),
        "notepad.exe matches the word `note` but is not the child's, and it was closed: the loop \
         is not passing the owner snapshot from SystemControl::child_processes"
    );
    assert!(
        running(&fake).contains(&DISCORD),
        "Discord.exe is the child's and matches `cord`, but it is a configured check program and \
         was closed: the loop is not passing the Integrations card's programs to decide"
    );
    let notices = fake.notification_bodies();
    assert!(
        notices
            .iter()
            .any(|n| n.contains("Games") && n.contains("Minecraft.exe")),
        "the child must be told which group's time ran out: {notices:?}"
    );

    let tally = std::fs::read_to_string(tmp.join("usage_state.json")).expect("tally written");
    assert!(
        !tally.contains(secret),
        "a visit's address reached usage_state.json: {tally}"
    );
}
