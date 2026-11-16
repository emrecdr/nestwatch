//! A whole bedtime, in order, through `curfew::run_enforcer`: the countdown, the shutdown, a
//! parent's extension calling it off, the countdown again before the extension runs out, and
//! bedtime back.
//!
//! The single-path tests each reach one ending: `curfew_countdown.rs` the warnings,
//! `curfew_enforcer.rs` the shutdown. Ordering is where this project's real defects have lived (the
//! wiring class the per-push mutation jobs in `ci.yml` catch), and one path cannot see a warning
//! said after the shutdown, a shutdown an extension did not call off, or bedtime that never came
//! back once the extension ran out. Each step here waits for its own effect before the next is
//! written, so the order is asserted, not just the set.
//!
//! Bedtime and the extension are moved rather than the clock, the way a parent's edits move them.
//!
//! Its own binary, like the other curfew drivers: one loop, one fake, nothing shared.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use nestwatch::config::Config;
use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::curfew::{Curfew, run_enforcer};
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, bedtime_in, wait_for};

/// Long enough that no tick in this test finds the grace period over and re-issues the shutdown
/// at once, which would add a call the sequence does not expect.
const GRACE_SECS: u32 = 600;

#[tokio::test]
async fn a_bedtime_is_counted_down_enforced_extended_and_enforced_again() {
    let curfew = |c: Curfew| Curfew {
        warn_secs: GRACE_SECS,
        ..c
    };
    let config = Arc::new(RwLock::new(Config {
        curfew: curfew(bedtime_in(20)),
        ..Default::default()
    }));
    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    let scratch = ScratchDir::new("curfew-sequence");
    let usage = Arc::new(UsageLog::new(scratch.join("usage.jsonl")));
    let (waker, wake) = tokio::sync::watch::channel(0u64);
    let loop_handle = tokio::spawn(run_enforcer(control, config.clone(), usage.clone(), wake));
    let heard = || fake.notification_bodies();
    let shutdowns = || fake.shutdowns();
    let write = |edit: &dyn Fn(&mut Curfew)| {
        edit(&mut config.write().unwrap().curfew);
        waker.send_modify(|n| *n += 1);
    };
    let bedtime_said = Some("Bedtime — this computer is shutting down.".to_string());

    // Twenty minutes off: past the countdown's horizon, so the first tick only takes a reading.
    assert!(
        wait_for(|| nestwatch::heartbeat::worst_age_secs().is_some()).await,
        "the loop never ticked"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(heard().is_empty(), "{:?}", heard());

    // The countdown, each warning once and in order.
    for (mins, said) in [
        (14, "Bedtime in 15 minutes."),
        (4, "Bedtime in 5 minutes — good time to save."),
        (1, "Bedtime in 1 minute!"),
    ] {
        let before = heard().len();
        write(&|c| *c = curfew(bedtime_in(mins)));
        assert!(
            wait_for(|| heard().len() > before).await,
            "nothing said {mins} minutes out: {:?}",
            heard()
        );
        assert_eq!(heard().last().map(String::as_str), Some(said));
    }
    assert!(shutdowns().is_empty(), "shut down before bedtime");

    // Bedtime: one shutdown, with the configured grace and the reason, and no fourth notice —
    // Windows' own countdown box carries the message from here.
    write(&|c| *c = curfew(bedtime_in(-1)));
    assert!(
        wait_for(|| !shutdowns().is_empty()).await,
        "no shutdown at bedtime"
    );
    assert_eq!(shutdowns(), vec![(GRACE_SECS, bedtime_said.clone())]);
    assert_eq!(
        heard().len(),
        3,
        "told the child something after the shutdown"
    );

    // Half an hour more, granted while the shutdown is pending: it is called off, and nothing is
    // said, because bedtime is now half an hour away, past what the countdown looks ahead to.
    let now = nestwatch::clock::now();
    write(&|c| c.extra_until = Some(now + chrono::Duration::minutes(30)));
    assert!(
        wait_for(|| fake.aborted_shutdowns() == 1).await,
        "the extension did not call off the pending shutdown"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(shutdowns().len(), 1, "shut down again during the extension");
    assert_eq!(
        heard().len(),
        3,
        "a warning during the first minute of the extension"
    );

    // Taken back to four minutes: the countdown runs again before bedtime returns, so the child
    // hears the five-minute warning rather than a shutdown out of nowhere.
    let now = nestwatch::clock::now();
    write(&|c| c.extra_until = Some(now + chrono::Duration::minutes(4)));
    assert!(
        wait_for(|| heard().len() == 4).await,
        "no warning before the extension runs out: {:?}",
        heard()
    );
    assert_eq!(
        heard()[3],
        "Bedtime in 5 minutes — good time to save.",
        "{:?}",
        heard()
    );

    // Run out: bedtime is back, with a fresh grace period rather than an immediate power-off,
    // because the last shutdown was called off rather than ignored.
    let now = nestwatch::clock::now();
    write(&|c| c.extra_until = Some(now - chrono::Duration::seconds(1)));
    assert!(
        wait_for(|| shutdowns().len() == 2).await,
        "bedtime did not come back when the extension ran out"
    );
    loop_handle.abort();
    assert_eq!(shutdowns()[1], (GRACE_SECS, bedtime_said));
    assert_eq!(fake.aborted_shutdowns(), 1);
    assert_eq!(heard().len(), 4, "{:?}", heard());

    // Each warning has its row beside it, and only the warnings do: the shutdowns and the abort
    // write none. Newest first, as the log returns them.
    let rows: Vec<serde_json::Value> = usage
        .recent(50)
        .into_iter()
        .filter(|r| r["event"] == "curfew_countdown")
        .map(|r| r["minutes_remaining"].clone())
        .collect();
    assert_eq!(rows, [5, 1, 5, 15].map(serde_json::Value::from));
}
