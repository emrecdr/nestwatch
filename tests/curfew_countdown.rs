//! The bedtime countdown, observed through `curfew::run_enforcer` itself.
//!
//! `Enforcer::tick` is unit-tested on every reading it can be handed, but the loop chooses which
//! reading to hand it: nothing to count down to while curfew is off, how far off bedtime is
//! otherwise. Nothing drove that choice (`docs/OPEN-FINDINGS.md` O109), so a loop that never
//! counted down while curfew was on passed, and so did one that kept counting while it was off —
//! which announces "15 minutes" the moment a parent switches curfew on close to bedtime.
//!
//! Bedtime is moved rather than the clock: each step writes a window starting a given number of
//! minutes from the trusted clock's now and wakes the loop, the way a parent's edit does.
//!
//! Its own binary, like `curfew_enforcer.rs`: one loop, one fake, nothing shared with another test.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use nestwatch::config::Config;
use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::curfew::{Curfew, run_enforcer};
use nestwatch::usage::UsageLog;

mod common;
use common::wait_for;

/// A curfew whose window opens `mins` minutes from now and lasts an hour.
fn bedtime_in(mins: i64) -> Curfew {
    let start = nestwatch::clock::now() + chrono::Duration::minutes(mins);
    let hm = |t: chrono::DateTime<chrono::FixedOffset>| t.format("%H:%M").to_string();
    Curfew {
        enabled: true,
        start: hm(start),
        end: hm(start + chrono::Duration::hours(1)),
        ..Default::default()
    }
}

#[tokio::test]
async fn bedtime_is_counted_down_while_curfew_is_on_and_not_announced_on_switching_it_on() {
    let config = Arc::new(RwLock::new(Config {
        curfew: bedtime_in(20),
        ..Default::default()
    }));
    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    let (waker, wake) = tokio::sync::watch::channel(0u64);
    let loop_handle = tokio::spawn(run_enforcer(
        control,
        config.clone(),
        Arc::new(UsageLog::disabled()),
        wake,
    ));
    let heard = || fake.notification_bodies();
    let set = |curfew: Curfew| {
        config.write().unwrap().curfew = curfew;
        waker.send_modify(|n| *n += 1);
    };

    // Twenty minutes off, past what the countdown looks ahead to: the first tick says nothing.
    assert!(
        wait_for(|| nestwatch::heartbeat::worst_age_secs().is_some()).await,
        "the loop never ticked"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        heard(),
        Vec::<String>::new(),
        "nothing to say twenty minutes out"
    );

    // Fourteen minutes off: the fifteen-minute warning.
    set(bedtime_in(14));
    assert!(
        wait_for(|| heard().len() == 1).await,
        "no countdown while curfew is on and bedtime is fourteen minutes away"
    );
    assert_eq!(heard()[0], "Bedtime in 15 minutes.");

    // Switched off, then on again with bedtime ten minutes away. Switching off re-primes the
    // countdown, so the first reading afterwards announces nothing: a "15 minutes" now would tell
    // the child about a threshold that passed while curfew was off.
    set(Curfew {
        enabled: false,
        ..bedtime_in(14)
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    set(bedtime_in(10));
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Four minutes off: the five-minute warning, which also shows the ten-minute reading was taken.
    set(bedtime_in(4));
    assert!(
        wait_for(|| heard().len() >= 2).await,
        "no five-minute warning: {:?}",
        heard()
    );
    loop_handle.abort();
    assert_eq!(
        heard(),
        vec![
            "Bedtime in 15 minutes.".to_string(),
            "Bedtime in 5 minutes — good time to save.".to_string(),
        ],
        "switching curfew back on near bedtime announced a threshold it had already passed"
    );
}
