//! What the child is actually told when screen time runs out on a **Shutdown**-configured install.
//!
//! Its own test binary, like `enforcer_loop.rs` and for the same reason: `NESTWATCH_DATA_DIR` is
//! process-global, so two `#[tokio::test]` functions sharing a binary would race over the override.
//!
//! # Why this drives the loop instead of testing the message functions
//!
//! Two real defects lived in this one string, and **both were invisible to unit tests**:
//!
//! * It was hard-coded English. A Dutch install showed a Dutch countdown, a Dutch lock warning,
//!   and then an English shutdown notice — at the most stressful moment the child gets.
//! * It was the only child-facing message that never carried the "where to ask for more time"
//!   address, so a Shutdown-configured install never told the child how to ask, while an otherwise
//!   identical Lock-configured one always did.
//!
//! Neither is arithmetic; both are **wiring**, which is the class `docs/OPEN-FINDINGS.md` O70 names
//! as this project's recurring failure and the reason it asks for a driver test. A test on
//! `shutdown_message(lang)` proves the string is translated and says nothing about whether the loop
//! calls it, and `FakeControl::shutdown` only wrote to `tracing`, so until now nothing *could*
//! observe the message at all.
//!
//! So this asserts on the bytes the child would really see: the loop's own decision, composed by
//! the loop's own code, arriving at `SystemControl::shutdown`.

use std::sync::{Arc, RwLock};

use nestwatch::config::{Language, data_paths};
use nestwatch::control::{FakeControl, SystemControl};
use nestwatch::foreground::Feed;
use nestwatch::rules::{EnforceAction, Rules, Usage, run_rules_enforcer};
use nestwatch::screentime::ScreentimeLog;
use nestwatch::usage::UsageLog;

mod common;
use common::{ScratchDir, idle_waker, test_config, wait_for};

/// The port this install is on — deliberately not 8443, so a hint that hard-coded the default
/// would fail here rather than pass by coincidence.
const PORT: u16 = 9443;

/// Write a tally that leaves the child well over budget the moment the loop starts.
///
/// Serialized from a real [`Usage`] rather than hand-written JSON so a field gaining a `serde`
/// attribute cannot make this fixture silently stop parsing — `load_or_default` swallows a parse
/// error and returns a zeroed tally, which would leave the child *under* budget and the test
/// waiting on a shutdown that never comes, for a reason having nothing to do with the assertion.
fn seed_spent_budget(total_secs: u64) {
    let usage = Usage {
        day: Some(nestwatch::config::today()),
        total_secs,
        ..Default::default()
    };
    let json = serde_json::to_string(&usage).expect("usage serializes");
    std::fs::write(data_paths().dir.join("usage_state.json"), json).expect("seeding the tally");
}

#[tokio::test]
async fn a_dutch_child_is_told_in_dutch_why_the_pc_is_shutting_down_and_where_to_ask() {
    let tmp = ScratchDir::new("enforcer-shutdown");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    // One minute of budget, ten minutes already spent: over budget on the very first tick, so the
    // test needs no elapsed time (see `enforcer_loop.rs` on why the clock is not paused).
    seed_spent_budget(600);

    let mut cfg = test_config();
    cfg.port = PORT;
    cfg.language = Language::Nl;
    cfg.rules = Rules {
        enabled: true,
        daily_budget_mins: 1,
        budget_action: EnforceAction::Shutdown,
        warn_secs: 45,
        ..Default::default()
    };
    // Curfew off, so `ask_hint` is live. During a curfew window it deliberately returns `None`,
    // because extra time cannot move bedtime — a separate rule with its own test.
    assert!(!cfg.curfew.enabled, "default curfew must be off here");

    let fake = Arc::new(FakeControl::new());
    let control: Arc<dyn SystemControl> = fake.clone();
    let loop_handle = tokio::spawn(run_rules_enforcer(
        control,
        Arc::new(RwLock::new(cfg)),
        Arc::new(UsageLog::disabled()),
        Arc::new(ScreentimeLog::disabled()),
        Feed::new(),
        idle_waker(),
    ));

    let arrived = wait_for(|| !fake.shutdowns().is_empty()).await;
    loop_handle.abort();
    assert!(
        arrived,
        "the enforcer never asked for a shutdown despite the budget being spent — the child would \
         have kept playing while the dashboard reported the limit as enforced"
    );

    let (delay, message) = fake.shutdowns()[0].clone();
    let message = message.expect("a shutdown the child can see must carry a reason, not a bare /s");

    assert_eq!(
        delay, 45,
        "the shutdown must use the configured warning countdown, or the child gets no grace"
    );
    assert!(
        message.contains("afgesloten"),
        "a Dutch install must explain the shutdown in Dutch. This is the one notice a \
         Shutdown-configured install shows, and it was English on every install: {message}"
    );
    assert!(
        !message.contains("shutting down"),
        "English leaked into the Dutch notice: {message}"
    );
    assert!(
        message.contains(&format!("https://localhost:{PORT}/ask")),
        "the notice must tell the child where to ask for more time, on the port this install \
         actually uses. A Lock-configured install says so and this one did not: {message}"
    );
}
