//! What the child is actually told when bedtime arrives.
//!
//! The sibling of `enforcer_shutdown.rs`, and it exists because writing that one and not this one
//! left a hole I then walked into.
//!
//! # The gap this closes
//!
//! `bedtime_shutdown_message(lang)` has a unit test proving each language gets its own wording, and
//! `translated_strings.rs` proves the notice is built by a function rather than written inline.
//! Neither proves the **call site** uses it. Swapping the call for `bedtime_title(lang)` — also a
//! function, also translated, also passing every check above — was measured to leave **all 507
//! tests green** while reducing the shutdown notice to the single word "Bedtijd", with nothing to
//! say the computer was about to power off.
//!
//! That is one of **five** instances of a single shape found in two days, across two sessions
//! working this repo — `api.rs`'s extension-preservation line, `install.rs`'s `ask_url` and
//! `alternate_note` call sites, this, and `app.js`'s `noteOtherLimit`. Every time, a pure helper is
//! easy to test and gets tested thoroughly, while the one line that calls it is reachable only from
//! a loop, a handler or an async method and gets nothing.
//! **The suite grows where testing is cheap, and the hole forms exactly where a silent revert does
//! the most damage.** `docs/OPEN-FINDINGS.md` O75 records the class and what to do about it.
//!
//! Its own binary, matching `enforcer_shutdown.rs`. This one needs no `NESTWATCH_DATA_DIR`: the
//! curfew loop reads config and writes only to a `UsageLog`, so it never touches the data
//! directory — unlike the rules enforcer, which persists a tally.

use std::sync::{Arc, RwLock};

use nestwatch::config::{Config, Language};
use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::curfew::{Curfew, run_enforcer};
use nestwatch::usage::UsageLog;

mod common;
use common::{idle_waker, wait_for};

/// A window that is open right now, whatever "now" happens to be on the machine running this.
///
/// Derived from the same trusted clock the enforcer reads, rather than hard-coded hours, so this
/// cannot pass all day and fail on the CI box that happens to run it at 03:00. The hour either
/// side wraps past midnight when it needs to, which `is_within` already handles and sweeps.
fn window_open_now() -> (String, String) {
    let now = nestwatch::clock::now();
    let fmt = |t: chrono::DateTime<chrono::FixedOffset>| t.format("%H:%M").to_string();
    (
        fmt(now - chrono::Duration::hours(1)),
        fmt(now + chrono::Duration::hours(1)),
    )
}

#[tokio::test]
async fn a_dutch_child_is_told_in_dutch_that_bedtime_is_shutting_the_pc_down() {
    let (start, end) = window_open_now();
    let mut cfg = Config {
        language: Language::Nl,
        ..Default::default()
    };
    cfg.curfew = Curfew {
        enabled: true,
        start,
        end,
        warn_secs: 45,
        ..Default::default()
    };

    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    let loop_handle = tokio::spawn(run_enforcer(
        control,
        Arc::new(RwLock::new(cfg)),
        Arc::new(UsageLog::disabled()),
        idle_waker(),
    ));

    let arrived = wait_for(|| !fake.shutdowns().is_empty()).await;
    loop_handle.abort();
    assert!(
        arrived,
        "the curfew enforcer never asked for a shutdown inside its own window — bedtime is not \
         being enforced at all"
    );

    let (delay, message) = fake.shutdowns()[0].clone();
    let message = message
        .expect("the child must be told why the machine is going off, not merely that it is");

    assert_eq!(
        delay, 45,
        "bedtime must use the configured warning countdown, or the child gets no grace"
    );
    assert!(
        message.contains("afgesloten"),
        "a Dutch install must say, in Dutch, that the computer is shutting down. This is the whole \
         notice — `shutdown.exe /c` is the only text on screen, with no toast beside it: {message}"
    );
    assert!(
        message.contains("bedtijd"),
        "and it must name the reason the child has been reading in the countdown for the last five \
         minutes: {message}"
    );
    assert!(
        !message.to_lowercase().contains("curfew"),
        "\"Curfew\" is the parent's word for the setting and appears nowhere the child can see: \
         {message}"
    );
}
