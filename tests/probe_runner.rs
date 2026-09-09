//! The probe scheduler's tick, driven by hand through the fake controller: a deposited secret
//! reaches the probe, its answer is judged by the registry, the interval and the ceiling bound
//! how often it runs, and a probe that fails or lies grants nothing.
//!
//! **One test, its own binary**, for the reason `earned_grant.rs` gives: every grant persists
//! the config, so this needs the process-wide `NESTWATCH_DATA_DIR` override.

use std::sync::Arc;

use chrono::{DateTime, Duration, FixedOffset};

use nestwatch::config::{Config, EarnedDay, Probe, Provider, Tier};
use nestwatch::control::FakeControl;
use nestwatch::probe::{self, ProbeOutcome, ProbeStatus, Progress};
use nestwatch::state::{AppState, recover_read, recover_write};

mod common;
use common::{ScratchDir, state_with, test_config};

fn at(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn status_of(state: &AppState, name: &str) -> ProbeStatus {
    state
        .probe_status
        .lock()
        .unwrap()
        .get(name)
        .cloned()
        .unwrap_or_else(|| panic!("no probe status for {name}"))
}

fn extra_today(state: &AppState, now: DateTime<FixedOffset>) -> u32 {
    recover_read(&state.config).extra.for_day(now.date_naive())
}

#[tokio::test]
async fn a_probe_is_run_judged_and_bounded_by_the_registry() {
    let tmp = ScratchDir::new("proberunner");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let fake = Arc::new(FakeControl::new());
    let mut cfg = test_config();
    cfg.providers.insert(
        "studygo".into(),
        Provider {
            enabled: true,
            minutes: 30,
            daily_cap_mins: Some(30),
            tiers: vec![
                Tier {
                    questions: 10,
                    minutes_practised: 20,
                    reward_mins: 16,
                },
                Tier {
                    questions: 15,
                    minutes_practised: 30,
                    reward_mins: 30,
                },
            ],
            probe: Some(Probe {
                exe: "studygo-probe".into(),
                every_mins: 15,
            }),
        },
    );
    cfg.providers.insert(
        "chores".into(),
        Provider {
            enabled: true,
            minutes: 20,
            daily_cap_mins: None,
            tiers: Vec::new(),
            probe: None,
        },
    );
    let mut state = state_with(cfg);
    state.control = fake.clone();
    let t0 = at("2026-09-08T16:00:00+02:00");
    let today = t0.date_naive();

    // --- A deposited secret reaches the probe on stdin, and its answer is judged by the ladder --
    probe::store_secret(
        &nestwatch::config::data_paths().dir,
        "studygo",
        b"session-token",
    )
    .unwrap();
    fake.script_probe(Ok(br#"{"questions":12,"minutes":5}"#.to_vec()));
    probe::run_once(&state, t0).await;
    let calls = fake.probe_calls();
    assert_eq!(calls.len(), 1, "one probe configured, one run");
    // Two assertions, not one, and the second is the load-bearing one. Comparing against
    // `probe_dir().join(..)` alone computes the expected value from the code under test, so it
    // agrees with itself however wrong `probe_dir()` is — a mutant that emptied it survived
    // exactly this line. `probe.rs` pins what that directory must be; this pins that the
    // scheduler ran the file the parent named, inside an absolute one.
    assert_eq!(calls[0].0, probe::probe_dir().join("studygo-probe"));
    assert_eq!(
        calls[0].0.file_name().and_then(|n| n.to_str()),
        Some("studygo-probe"),
        "the file the parent named is the file that ran"
    );
    assert!(
        calls[0].0.is_absolute(),
        "resolved, not relative: {:?}",
        calls[0].0
    );
    assert_eq!(calls[0].1, b"session-token", "the secret travels on stdin");
    assert_eq!(
        extra_today(&state, t0),
        16,
        "12 questions clears the lower rung"
    );
    assert_eq!(
        recover_read(&state.config).earned["studygo"],
        EarnedDay {
            date: today,
            minutes: Some(16)
        }
    );
    let status = status_of(&state, "studygo");
    assert_eq!(status.outcome, ProbeOutcome::Granted(16));
    assert_eq!(
        status.reported,
        Some(Progress {
            questions: 12,
            minutes: 5
        })
    );
    assert_eq!(status.at, t0);
    assert_eq!(
        Config::load().unwrap().extra.for_day(today),
        16,
        "a probe grant is persisted like a pushed one"
    );

    // --- Not due again inside its interval ----------------------------------------------------
    probe::run_once(&state, t0 + Duration::minutes(14)).await;
    assert_eq!(fake.probe_calls().len(), 1);

    // --- Due after it: more work tops up to the ceiling ---------------------------------------
    fake.script_probe(Ok(br#"{"questions":15,"minutes":5}"#.to_vec()));
    probe::run_once(&state, t0 + Duration::minutes(15)).await;
    assert_eq!(fake.probe_calls().len(), 2);
    assert_eq!(extra_today(&state, t0), 30);
    assert_eq!(
        status_of(&state, "studygo").outcome,
        ProbeOutcome::Granted(14)
    );

    // --- Paid in full: not even run ----------------------------------------------------------
    probe::run_once(&state, t0 + Duration::minutes(30)).await;
    assert_eq!(
        fake.probe_calls().len(),
        2,
        "a source paid in full is not polled again today"
    );

    // --- Tomorrow it is due again, and yesterday's entry does not exhaust it ------------------
    let tomorrow = t0 + Duration::days(1);
    fake.script_probe(Ok(br#"{"questions":3,"minutes":1}"#.to_vec()));
    probe::run_once(&state, tomorrow).await;
    assert_eq!(fake.probe_calls().len(), 3);
    assert_eq!(
        status_of(&state, "studygo").outcome,
        ProbeOutcome::Refused("below_threshold")
    );
    assert_eq!(extra_today(&state, tomorrow), 0);

    // --- A probe that fails, or answers nonsense, is recorded and grants nothing --------------
    fake.script_probe(Err("no network".into()));
    probe::run_once(&state, tomorrow + Duration::minutes(15)).await;
    match status_of(&state, "studygo").outcome {
        ProbeOutcome::Failed(message) => assert!(message.contains("no network"), "{message}"),
        other => panic!("expected a failure, got {other:?}"),
    }
    fake.script_probe(Ok(b"<html>sign in again</html>".to_vec()));
    probe::run_once(&state, tomorrow + Duration::minutes(30)).await;
    assert!(matches!(
        status_of(&state, "studygo").outcome,
        ProbeOutcome::Failed(_)
    ));
    assert_eq!(status_of(&state, "studygo").reported, None);
    assert_eq!(extra_today(&state, tomorrow), 0);

    // --- Switched off: not run ---------------------------------------------------------------
    recover_write(&state.config)
        .providers
        .get_mut("studygo")
        .unwrap()
        .enabled = false;
    fake.script_probe(Ok(br#"{"questions":50,"minutes":50}"#.to_vec()));
    probe::run_once(&state, tomorrow + Duration::minutes(45)).await;
    assert_eq!(fake.probe_calls().len(), 5);
    assert_eq!(extra_today(&state, tomorrow), 0);

    // --- Nothing runs while he is not at the machine ------------------------------------------
    //
    // Not a detail: a probe exists to ask what he has practised, and there is no session to launch
    // into when he is signed out. It is also the one place this codebase deliberately fails the
    // *other* way from the enforcers — they treat an unreadable session state as `Active` so a
    // failure never hands out unlimited time, and this treats it as "do not run" so a failure never
    // spends a request on a third party's API on the strength of a state it could not read.
    recover_write(&state.config)
        .providers
        .get_mut("studygo")
        .unwrap()
        .enabled = true;
    let before = fake.probe_calls().len();
    for state_now in [
        nestwatch::control::SessionState::Locked,
        nestwatch::control::SessionState::NoUser,
    ] {
        fake.script_session_state(Ok(state_now));
        probe::run_once(&state, tomorrow + Duration::minutes(60)).await;
        assert_eq!(
            fake.probe_calls().len(),
            before,
            "no probe may run while the session is {state_now:?}"
        );
    }
    fake.script_session_state(Err("cannot tell".into()));
    probe::run_once(&state, tomorrow + Duration::minutes(75)).await;
    assert_eq!(
        fake.probe_calls().len(),
        before,
        "nor when the session state cannot be read at all"
    );
    fake.script_session_state(Ok(nestwatch::control::SessionState::Active));
    fake.script_probe(Ok(br#"{"questions":40,"minutes":40}"#.to_vec()));
    probe::run_once(&state, tomorrow + Duration::minutes(90)).await;
    assert_eq!(
        fake.probe_calls().len(),
        before + 1,
        "and it runs again the moment he is back"
    );

    // --- Nothing configured means nothing happens --------------------------------------------
    let fake = Arc::new(FakeControl::new());
    let mut cfg = test_config();
    cfg.providers.insert(
        "chores".into(),
        Provider {
            enabled: true,
            minutes: 20,
            daily_cap_mins: None,
            tiers: Vec::new(),
            probe: None,
        },
    );
    let mut state = state_with(cfg);
    state.control = fake.clone();
    probe::run_once(&state, t0).await;
    assert!(fake.probe_calls().is_empty());
    assert!(state.probe_status.lock().unwrap().is_empty());
    assert_eq!(extra_today(&state, t0), 0);
}
