//! That the enforcer loop writes the trusted clock's high-water mark to disk — the join between
//! `clock::mark` and the tick that persists it, which no unit test reaches.
//!
//! `O82`: the mark lived only in memory, so a zone changed before a reboot left the tamper
//! fallback at the install-time offset for the rest of the install — an hour behind all summer.
//! `clock.rs` pins what the mark means and how it is restored; this pins that it reaches disk at all.
//!
//! Its own binary for two reasons: `NESTWATCH_DATA_DIR` is process-global, and so is the clock
//! anchor this test installs. The anchor is the host's **current** offset with no zone recorded,
//! which `clock::decide` answers with "believe the OS" — so nothing else in the process changes
//! day, and the only observable is the file.

use std::sync::{Arc, RwLock};

use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::rules::{Rules, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, idle_waker, test_config, wait_for};

#[tokio::test]
async fn the_enforcer_tick_writes_the_clock_mark_beside_its_anchor() {
    let tmp = ScratchDir::new("clock-mark");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };
    let path = nestwatch::clock::mark_path();
    assert!(!path.exists(), "the scratch data dir must start empty");

    let here = nestwatch::clock::current_offset_mins();
    nestwatch::clock::set_anchor(here);

    let control: Arc<dyn SystemControl> = Arc::new(FakeControl::new());
    let loop_handle = tokio::spawn(run_rules_enforcer(
        control,
        Arc::new(RwLock::new({
            let mut cfg = test_config();
            // Paused on purpose: the mark is about the machine's clock, not the child, so it has
            // to reach disk on every tick — a household that pauses for a fortnight across a DST
            // change and then reboots must not lose the summer.
            cfg.rules = Rules {
                enabled: false,
                daily_budget_mins: 60,
                ..Default::default()
            };
            cfg
        })),
        Arc::new(UsageLog::disabled()),
        Arc::new(ScreentimeLog::disabled()),
        nestwatch::probe::gate_ceiling(Default::default()),
        Feed::new(),
        idle_waker(),
    ));

    let written = wait_for(|| path.exists()).await;
    loop_handle.abort();
    assert!(
        written,
        "the loop ticked and never wrote {}: the mark would not survive a reboot",
        path.display()
    );
    assert_eq!(
        nestwatch::clock::load_mark(&path),
        Some(nestwatch::clock::Mark {
            anchor_mins: here,
            high_water_mins: here,
        }),
        "a fresh anchor's mark is the anchor itself, recorded beside it"
    );
}
