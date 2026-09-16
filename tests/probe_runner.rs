//! The probe scheduler's tick, driven by hand through the fake controller: a deposited secret
//! reaches the probe, its answer is judged by the registry, the interval and the ceiling bound
//! how often it runs, and a probe that fails or lies grants nothing.
//!
//! **One test, its own binary**, for the reason `earned_grant.rs` gives: every grant persists
//! the config, so this needs the process-wide `NESTWATCH_DATA_DIR` override.

use std::sync::Arc;

use chrono::{DateTime, Duration, FixedOffset};

use nestwatch::config::{Config, EarnedDay, Probe, Provider, Refused, Tier};
use nestwatch::control::FakeControl;
use nestwatch::probe::{self, ProbeOutcome, ProbeStatus, Progress};
use nestwatch::state::{AppState, recover_read, recover_write};

mod common;
use common::{ScratchDir, idle_waker, state_with, test_config, wait_for};

/// The laddered `studygo` provider these sections use: two rungs, a ceiling at the top one, and a
/// probe every fifteen minutes.
///
/// A function rather than a second copy of the literal. The two copies this replaces were
/// byte-identical and two hundred lines apart, and the later section asserts against *their*
/// numbers — `contains("10")`, `contains("16")` — so editing one ladder would have left the other
/// half of this test quietly asserting a fixture nothing else used.
fn laddered_studygo() -> Provider {
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
            first_check_after_mins: 0,
        }),
        remind_every_check: false,
        gate: None,
    }
}

/// The push-only provider beside it: no ladder, no ceiling, no probe.
fn plain_chores() -> Provider {
    Provider {
        enabled: true,
        minutes: 20,
        daily_cap_mins: None,
        tiers: Vec::new(),
        probe: None,
        remind_every_check: false,
        gate: None,
    }
}

fn at(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

/// Write the day's screen-time tally the settling period is measured against.
///
/// Serialized from a real [`nestwatch::rules::Usage`] rather than hand-written JSON, for the
/// reason `enforcer_shutdown.rs` gives where it does the same: `load_or_default` swallows a parse
/// error and hands back a zeroed tally, so a field gaining a `serde` attribute would turn this
/// fixture into "no time used" — which is the *refused* side of every assertion below, and would
/// pass while proving nothing.
fn seed_used(day: chrono::NaiveDate, mins: u64) {
    let usage = nestwatch::rules::Usage {
        day: Some(day),
        total_secs: mins * 60,
        ..Default::default()
    };
    std::fs::write(
        nestwatch::config::data_paths().dir.join("usage_state.json"),
        serde_json::to_string(&usage).expect("usage serializes"),
    )
    .expect("seeding the tally");
}

fn status_of(state: &AppState, name: &str) -> ProbeStatus {
    state
        .probe_status
        .lock()
        .unwrap()
        .get(name)
        .map(|entry| entry.last.clone())
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

    // --- The scheduler reports itself alive, and does not speak for the enforcers -------------
    //
    // Was `O102`: a dead scheduler was indistinguishable from "nothing was due", and both readings
    // land on the same stale line. It stamps its own cell now — but deliberately NOT one the
    // *enforcement alive* banner reads, because a dead probe loop is not stopped enforcement and
    // a banner that says it is would teach a parent to ignore the one sentence that means limits
    // are off. This binary is the place that can prove the second half: neither enforcer ever
    // runs here, so `worst_age_secs` staying `None` while the probe cell fills is the property
    // itself rather than a reading of the match arm.
    assert!(
        nestwatch::heartbeat::age_secs(nestwatch::heartbeat::Enforcer::Probe).is_none(),
        "a fresh binary must start with no probe heartbeat, or what follows proves nothing"
    );
    {
        let scheduler = tokio::spawn(probe::run_scheduler(
            state_with(test_config()),
            idle_waker(),
        ));
        assert!(
            wait_for(|| {
                nestwatch::heartbeat::age_secs(nestwatch::heartbeat::Enforcer::Probe).is_some()
            })
            .await,
            "the scheduler never stamped a heartbeat — a parent would have no way to tell a dead \
             loop from a quiet one"
        );
        assert!(
            nestwatch::heartbeat::worst_age_secs().is_none(),
            "the probe loop must not report itself as enforcement: nothing enforcing has ticked \
             in this binary, and the banner that says so must still say so"
        );
        scheduler.abort();
    }

    let fake = Arc::new(FakeControl::new());
    let mut cfg = test_config();
    cfg.providers.insert("studygo".into(), laddered_studygo());
    cfg.providers.insert("chores".into(), plain_chores());
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
            minutes: Some(16),
            bar_met: false
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
        ProbeOutcome::Refused(Refused::BelowThreshold)
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

    // --- What the child is told, and how often ------------------------------------------------
    //
    // Its own state, so the count is about this section rather than about everything above it.
    // The rule: the reminder is said at most once a local day, a grant is announced every time,
    // and a broken link is never his problem to read about.
    {
        let fake = Arc::new(FakeControl::new());
        let mut cfg = test_config();
        cfg.providers.insert("studygo".into(), laddered_studygo());
        let mut state = state_with(cfg);
        state.control = fake.clone();

        // Short of the first rung: one notice, naming the nearest rung and what he has done.
        fake.script_probe(Ok(br#"{"questions":3,"minutes":5}"#.to_vec()));
        probe::run_once(&state, t0).await;
        let said = fake.notification_bodies();
        assert_eq!(said.len(), 1, "one notice, not one per check: {said:?}");
        assert!(said[0].contains("studygo"), "{}", said[0]);
        assert!(
            said[0].contains("10"),
            "names the nearest rung: {}",
            said[0]
        );
        assert!(said[0].contains("16"), "and what it is worth: {}", said[0]);
        assert!(said[0].contains('3'), "and what he has done: {}", said[0]);

        // Still short, later the same day: nothing more. This is the whole point of the rule —
        // a notice every fifteen minutes is nagging, which the design refuses.
        fake.script_probe(Ok(br#"{"questions":4,"minutes":6}"#.to_vec()));
        probe::run_once(&state, t0 + Duration::minutes(15)).await;
        assert_eq!(
            fake.notification_bodies().len(),
            1,
            "the reminder is once a day, however many times it checks"
        );

        // A grant is announced every time, because a rule that only ever says "not yet" is the
        // controlling frame this feature was designed against.
        fake.script_probe(Ok(br#"{"questions":12,"minutes":5}"#.to_vec()));
        probe::run_once(&state, t0 + Duration::minutes(30)).await;
        let said = fake.notification_bodies();
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(
            said[1].contains("16"),
            "says how much was added: {}",
            said[1]
        );

        // A broken link says nothing to him: it is the parent's to fix, and it is on their card.
        fake.script_probe(Err("no network".into()));
        probe::run_once(&state, t0 + Duration::minutes(45)).await;
        assert_eq!(
            fake.notification_bodies().len(),
            2,
            "a failed check is not the child's problem to read"
        );

        // Tomorrow the reminder is available again.
        fake.script_probe(Ok(br#"{"questions":1,"minutes":1}"#.to_vec()));
        probe::run_once(&state, t0 + Duration::days(1)).await;
        assert_eq!(
            fake.notification_bodies().len(),
            3,
            "a new day earns a new reminder"
        );

        // A reminder the OS would not show is not a reminder he got, so it is offered again at
        // the next check rather than counted as said. The same distinction the countdown warnings
        // draw before recording one.
        let day_after = t0 + Duration::days(2);
        fake.fail_notifications("no interactive session");
        // Scripted once for both runs: `FakeControl::run_probe` clones its answer rather than
        // taking it, so a second identical call would configure nothing.
        fake.script_probe(Ok(br#"{"questions":2,"minutes":2}"#.to_vec()));
        probe::run_once(&state, day_after).await;
        assert_eq!(
            fake.notification_bodies().len(),
            4,
            "attempted, and refused by the OS"
        );
        probe::run_once(&state, day_after + Duration::minutes(15)).await;
        // A literal rather than the previous reading plus one: an expected value taken from the
        // thing under test agrees with it however wrong it is, and this file writes its counts out
        // everywhere else for that reason.
        assert_eq!(
            fake.notification_bodies().len(),
            5,
            "an undelivered reminder must be tried again, not marked as said"
        );
    }

    // --- A household that wants to be told at every check can be ------------------------------
    //
    // Once a day stays the default, and the section above is why: rationing is what keeps a
    // fifteen-minute timer from becoming a fifteen-minute nag. But that argument was made for a
    // reward — *there is more to earn if you want it* — and it does not survive being pointed at
    // a gate, where the notice is the only warning that the machine is about to lock. Which of
    // the two a household has is not something this file can know, so it is the parent's switch,
    // per provider, and absent means exactly today's behaviour.
    {
        let fake = Arc::new(FakeControl::new());
        let mut cfg = test_config();
        let mut every_check = laddered_studygo();
        every_check.remind_every_check = true;
        cfg.providers.insert("studygo".into(), every_check);
        let mut state = state_with(cfg);
        state.control = fake.clone();

        fake.script_probe(Ok(br#"{"questions":3,"minutes":5}"#.to_vec()));
        probe::run_once(&state, t0).await;
        assert_eq!(
            fake.notification_bodies().len(),
            1,
            "the first check speaks either way"
        );

        // The line that differs from the section above, and the only one that does.
        fake.script_probe(Ok(br#"{"questions":4,"minutes":6}"#.to_vec()));
        probe::run_once(&state, t0 + Duration::minutes(15)).await;
        let said = fake.notification_bodies();
        assert_eq!(
            said.len(),
            2,
            "with the switch on, every check that finds him short says so: {said:?}"
        );
        assert!(
            said[1].contains('4'),
            "and says what he has done NOW, not what the first notice said: {}",
            said[1]
        );

        // What the switch does not touch. A broken link is still the parent's to read...
        fake.script_probe(Err("no network".into()));
        probe::run_once(&state, t0 + Duration::minutes(30)).await;
        assert_eq!(
            fake.notification_bodies().len(),
            2,
            "a failed check is not the child's problem however the switch is set"
        );

        // ...and a grant is still announced, which it was before the switch existed.
        fake.script_probe(Ok(br#"{"questions":12,"minutes":5}"#.to_vec()));
        probe::run_once(&state, t0 + Duration::minutes(45)).await;
        let said = fake.notification_bodies();
        assert_eq!(said.len(), 3, "{said:?}");
        assert!(said[2].contains("16"), "says what was added: {}", said[2]);
    }

    // --- The first check of the day waits, and waits on screen time ---------------------------
    //
    // The household rule this gate was built towards opens with a settling period: he turns the
    // machine on and has a few minutes before anything asks what he has practised. Without one the
    // first check lands in the minute he signs in, and with the notice rationed to once a day (the
    // default, two sections up) that is the day's only warning spent on a child who has not had
    // time to open anything.
    //
    // **Measured in screen time used today, not in wall clock, and that is the load-bearing
    // choice.** A period measured from when the session became active is one the child controls:
    // signing out and back in every two minutes would mean the probe never ran at all, and the
    // gate would be defeated by the Start menu. Screen time only goes up, and it is the same
    // number the budget is spent against, so "three minutes in" means the same thing to both.
    {
        let fake = Arc::new(FakeControl::new());
        let mut cfg = test_config();
        let mut settles = laddered_studygo();
        settles.probe = Some(Probe {
            exe: "studygo-probe".into(),
            every_mins: 8,
            first_check_after_mins: 3,
        });
        cfg.providers.insert("studygo".into(), settles);
        let mut state = state_with(cfg);
        state.control = fake.clone();
        let day = t0.date_naive();

        // Nothing used yet: he has just signed in, and nothing is asked of him.
        seed_used(day, 0);
        fake.script_probe(Ok(br#"{"questions":12,"minutes":5}"#.to_vec()));
        probe::run_once(&state, t0).await;
        assert!(
            fake.probe_calls().is_empty(),
            "the first check ran inside the settling period"
        );
        assert_eq!(extra_today(&state, t0), 0);

        // Half an hour of wall clock later, with two minutes of it actually used — still inside.
        // This is the assertion that separates the two clocks; every other line here would pass
        // with either.
        seed_used(day, 2);
        probe::run_once(&state, t0 + Duration::minutes(30)).await;
        assert!(
            fake.probe_calls().is_empty(),
            "thirty minutes of wall clock and two of screen time is two minutes of screen time"
        );

        // Exactly at the boundary it runs. Both sides of the bound are asserted deliberately: a
        // `>` mutated to `>=` (or back) moves only this line, and the refused side above passes
        // under either.
        seed_used(day, 3);
        probe::run_once(&state, t0 + Duration::minutes(31)).await;
        assert_eq!(
            fake.probe_calls().len(),
            1,
            "at three minutes of screen time the first check must run"
        );
        assert_eq!(
            extra_today(&state, t0),
            16,
            "and its answer is judged as any other"
        );

        // Afterwards the interval governs alone. Not because anything remembers that the first
        // check has happened — it is a plain floor, checked every time — but because the tally it
        // reads only rises, so a floor already passed cannot delay anything again today.
        fake.script_probe(Ok(br#"{"questions":13,"minutes":6}"#.to_vec()));
        probe::run_once(&state, t0 + Duration::minutes(35)).await;
        assert_eq!(
            fake.probe_calls().len(),
            1,
            "four minutes after a check, with an interval of eight, is not due"
        );
        probe::run_once(&state, t0 + Duration::minutes(39)).await;
        assert_eq!(
            fake.probe_calls().len(),
            2,
            "eight minutes after a check is due, settling period or not"
        );

        // And a new day brings a new settling period — for the same reason, read the other way:
        // the tally resets with the day, so the floor is back below him. This is the half that
        // makes "a settling period each day" true without anything tracking days.
        let tomorrow = t0 + Duration::days(1);
        seed_used(tomorrow.date_naive(), 0);
        probe::run_once(&state, tomorrow).await;
        assert_eq!(
            fake.probe_calls().len(),
            2,
            "a new day starts a new settling period"
        );
        seed_used(tomorrow.date_naive(), 3);
        probe::run_once(&state, tomorrow + Duration::minutes(5)).await;
        assert_eq!(
            fake.probe_calls().len(),
            3,
            "and ends it the same way the first one did"
        );
    }

    // --- A gate is lifted by a check that cannot run, and by nothing else ---------------------
    //
    // `probe::providers_not_checking` is the one input to the ceiling that is not in the config,
    // and it is the one the household chose deliberately: *"we cannot tell"* is not *"he has not
    // practised"*, so an outage must not cost him his day. Driven through a real failing probe
    // rather than by hand-building a status map, because the thing under test is which OUTCOME
    // counts, and only a real run produces one.
    {
        let fake = Arc::new(FakeControl::new());
        let mut cfg = test_config();
        let mut gated = laddered_studygo();
        gated.gate = Some(nestwatch::config::Gate {
            allowance_mins: 35,
            questions: 15,
            minutes_practised: 30,
        });
        cfg.providers.insert("studygo".into(), gated.clone());
        // A gated provider with no probe: nothing to observe, so it is never in the set. Its
        // checking arrives as a push, and a push that has not come is indistinguishable from a
        // child who has not practised — which is the honest answer for that shape.
        let mut push_only = gated.clone();
        push_only.probe = None;
        cfg.providers.insert("pushonly".into(), push_only);
        // And a probe with no gate: nothing to lift, so it is never in the set either.
        cfg.providers.insert("chores".into(), {
            let mut p = laddered_studygo();
            p.gate = None;
            p
        });
        let mut state = state_with(cfg);
        state.control = fake.clone();
        let not_checking = |state: &AppState, scheduler_stale: bool| {
            nestwatch::probe::providers_not_checking(
                &recover_read(&state.config),
                &state.probe_status,
                scheduler_stale,
            )
        };

        // Before any check has run, nothing is known to be broken — and the gate binds, which is
        // the right default: he starts his day under it.
        assert!(
            not_checking(&state, false).is_empty(),
            "a probe that has not run yet is not a probe that failed"
        );

        fake.script_probe(Err("no network".into()));
        probe::run_once(&state, t0).await;
        let broken = not_checking(&state, false);
        assert!(
            broken.contains("studygo"),
            "a failed check must lift the gate: {broken:?}"
        );
        assert!(
            !broken.contains("pushonly") && !broken.contains("chores"),
            "only a gated provider with an observable check belongs here: {broken:?}"
        );

        // And it comes back the moment the check works again. A gate that stayed lifted after one
        // bad minute would be a gate a child could open once and leave open.
        fake.script_probe(Ok(br#"{"questions":1,"minutes":1}"#.to_vec()));
        probe::run_once(&state, t0 + Duration::minutes(20)).await;
        assert!(
            not_checking(&state, false).is_empty(),
            "a check that worked is not a check that cannot run"
        );

        // A dead scheduler lifts every gate it could have been checking — and **only** those. The
        // second half is why the staleness is a parameter rather than a process global read in
        // here: a gated provider with no probe has no scheduler to be waiting on, so a stopped
        // loop says nothing about it, and the mutant that dropped that distinction survived while
        // no test could reach the state that shows it.
        let stopped = not_checking(&state, true);
        assert!(
            stopped.contains("studygo"),
            "a stopped scheduler must lift the gate it was checking: {stopped:?}"
        );
        assert!(
            !stopped.contains("pushonly"),
            "but not a gate whose checking never went through the scheduler: {stopped:?}"
        );
        assert!(
            !stopped.contains("chores"),
            "and not a provider with no gate to lift: {stopped:?}"
        );
    }

    // --- Nothing configured means nothing happens --------------------------------------------
    let fake = Arc::new(FakeControl::new());
    let mut cfg = test_config();
    cfg.providers.insert("chores".into(), plain_chores());
    let mut state = state_with(cfg);
    state.control = fake.clone();
    probe::run_once(&state, t0).await;
    assert!(fake.probe_calls().is_empty());
    assert!(state.probe_status.lock().unwrap().is_empty());
    assert_eq!(extra_today(&state, t0), 0);
}
