//! Golden samples of every JSON shape the Android client parses.
//!
//! ## Why this exists
//!
//! The client in `../nestwatch-mobile` was written against a prose description of these
//! endpoints. Two implementations built from one document agree *because both agents read the
//! same sentences* — so a misreading produces two matching mistakes, and every test on both
//! sides stays green. Nothing in either repository currently checks the seam between them.
//!
//! These files are that check. They are produced **from the real serde types**, so renaming a
//! field or changing its type fails this test at the moment it happens. The client's
//! `models_test.dart` parses these same files, so the same rename fails over there too.
//!
//! ## Why the client cannot do this alone
//!
//! A fixture captured from a running server is a snapshot of what the server *did*, not a
//! contract. Rename a field on both sides and the captured file still parses the client's
//! parser: both suites pass, the seam is exactly as unchecked as prose was, and it now *looks*
//! mechanical. That is worse than having nothing, because it converts an acknowledged gap into
//! a hidden one. The producer half has to live here, next to the types.
//!
//! ## Updating
//!
//! A deliberate shape change is a two-line ritual: `UPDATE_GOLDEN=1 cargo test --test golden`,
//! then read the diff before committing it. **Read it.** These files are a contract with code in
//! another repository, and regenerating without looking is how the contract silently becomes
//! whatever the code happens to do — which is the failure this whole file exists to prevent.
//! A missing file is a failure rather than an auto-write, so deleting one cannot make the test
//! pass.

use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::http::StatusCode;
use chrono::NaiveDate;
use serde_json::{Value, json};

use nestwatch::rules::{Rules, Usage, today_summary};
use nestwatch::timecode::{ActiveCode, TimeCodes};
use nestwatch::timereq::PendingRequest;

mod common;

/// Compare `value` against `tests/golden/{name}.json`, or rewrite it under `UPDATE_GOLDEN=1`.
fn golden(name: &str, value: &Value) {
    let path = PathBuf::from("tests/golden").join(format!("{name}.json"));
    // Pretty-printed and newline-terminated so the files are readable in a diff and by the
    // client's maintainer, who should never have to run this suite to see what we send.
    let produced = format!("{}\n", serde_json::to_string_pretty(value).unwrap());

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &produced).unwrap();
        return;
    }

    let Ok(committed) = std::fs::read_to_string(&path) else {
        panic!(
            "{} is missing. If this shape is new, run `UPDATE_GOLDEN=1 cargo test --test golden` \
             and commit the file. Never create it by hand — the point is that it comes from the \
             types.\n\nWhat the types produce now:\n{produced}",
            path.display()
        );
    };

    assert_eq!(
        committed.trim_end(),
        produced.trim_end(),
        "\n\n{} no longer matches what the types produce.\n\n\
         If you renamed a field or changed a type, the Android client parses this exact file and \
         is now broken — fix it there before regenerating here. If the change is deliberate and \
         the client is ready, run `UPDATE_GOLDEN=1 cargo test --test golden`.\n",
        path.display()
    );
}

/// `GET /api/time-requests` → the parent-facing pending queue.
#[test]
fn time_requests() {
    let pending = vec![
        PendingRequest {
            id: "1993f2c8a10-3".into(),
            ts: "2026-08-26T18:14:02.117Z".into(),
            minutes: 30,
            reason: "finish the level".into(),
        },
        PendingRequest {
            id: "1993f2c1e40-2".into(),
            ts: "2026-08-26T18:11:47.882Z".into(),
            minutes: 15,
            reason: String::new(),
        },
    ];
    // The second carries an empty reason on purpose: the child's page allows it, and a client
    // that renders it as a blank line rather than substituting its own sentence looks broken.
    golden("time-requests", &serde_json::to_value(&pending).unwrap());
    golden(
        "time-requests-empty",
        &serde_json::to_value(Vec::<PendingRequest>::new()).unwrap(),
    );
}

/// `GET /api/time-codes` → active, unredeemed offline codes.
#[test]
fn time_codes() {
    let codes = vec![ActiveCode {
        code: "K7M2QF".into(),
        ts: "2026-08-26T17:02:11.004Z".into(),
        minutes: 45,
    }];

    // The sample's LENGTH is derived, not typed — because a literal cannot fail when the constant
    // moves, and this file proved it: it carried an eight-character code for a day after codes
    // became six, regenerating unchanged the whole time because the length lives in the source
    // above rather than in `CODE_LEN`.
    //
    // `timecode`'s own unit test pins `CODE_LEN` to a literal deliberately — asserting a constant
    // against itself pins nothing — so a change there fails *there*. What it cannot notice is this
    // file, and this is the one a client sizes an input box and a reveal mask against. A sample
    // that disagrees with what the server issues is a contract that lies, so it is checked against
    // a real issued code rather than trusted.
    let dir = std::env::temp_dir().join(format!("nw-golden-code-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let issued = TimeCodes::new(dir.join("time_codes.jsonl"))
        .issue(45)
        .expect("a fresh store must be able to issue one code");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        codes[0].code.len(),
        issued.len(),
        "the sample code is {} characters but the server issues {} — the Android client sizes its \
         input and its reveal mask against this file, so regenerate the sample to match rather \
         than shipping a contract that disagrees with the server",
        codes[0].code.len(),
        issued.len()
    );

    golden("time-codes", &serde_json::to_value(&codes).unwrap());
    golden(
        "time-codes-empty",
        &serde_json::to_value(Vec::<ActiveCode>::new()).unwrap(),
    );
}

/// `GET /api/usage/today` → today's summary.
///
/// Two samples, because the three distinctions a prose description loses are all absences, and
/// a sample that only ever shows the populated case cannot pin any of them:
///
/// * `remaining_mins` is `null` under an unlimited budget, and `0` means "budget fully spent".
///   A client rendering `null` as `0` tells a child with no limit they have none left.
/// * `enforcer_age_secs` is `null` when no enforcer has reached a tick. Defaulting it to `0`
///   reports healthy enforcement for a dead one — the worst direction to be wrong in.
/// * `focus_missing` separates "focused nothing" from "nothing was watching". An empty
///   `focused` list cannot carry that on its own, which is the whole reason the flag exists.
#[test]
fn usage_today() {
    let day = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();

    let rules = Rules {
        daily_budget_mins: 120,
        app_limits: BTreeMap::from([("minecraft".into(), 60)]),
        ..Rules::default()
    };
    let usage = Usage {
        day: Some(day),
        total_secs: 3_300,
        per_app_secs: BTreeMap::from([("minecraft".into(), 2_400)]),
        per_group_secs: BTreeMap::new(),
        foreground_secs: BTreeMap::from([("minecraft".into(), 2_400), ("chrome".into(), 900)]),
        page_secs: BTreeMap::from([("Poki - Free Online Games".into(), 780)]),
    };
    golden(
        "usage-today",
        &today_summary(&rules, day, 15, &usage, Some(12)),
    );

    // Unlimited budget, no enforcer heartbeat, and long enough use that an empty focus map
    // means nothing was *watching* rather than nothing being *used* (the floor is 300s).
    let unlimited = Rules {
        daily_budget_mins: 0,
        ..Rules::default()
    };
    let unwatched = Usage {
        day: None,
        total_secs: 600,
        per_app_secs: BTreeMap::new(),
        per_group_secs: BTreeMap::new(),
        foreground_secs: BTreeMap::new(),
        page_secs: BTreeMap::new(),
    };
    golden(
        "usage-today-unmeasured",
        &today_summary(&unlimited, day, 0, &unwatched, None),
    );
}

/// `GET /session` → `{authenticated, version}`.
///
/// Taken through the real router rather than by rebuilding the literal here, because this shape
/// has no named type — it is assembled inside the handler, so a test that reconstructs it would
/// be a mirror of itself and would agree with the handler no matter what the handler did.
///
/// The version *value* is normalised away. It changes every release, and pinning it would turn
/// every version bump into a false failure while adding nothing: what is contracted is that the
/// key exists and carries a string, not which string.
#[tokio::test]
async fn session() {
    let app = common::test_app();

    let response = common::get(&app, "/session", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut signed_out = common::body_json(response).await;
    signed_out["version"] = json!("<version>");
    golden("session-signed-out", &signed_out);

    let cookie = common::login(&app, common::PASSWORD)
        .await
        .expect("the test password should sign in");
    let response = common::get(&app, "/session", Some(&cookie)).await;
    let mut signed_in = common::body_json(response).await;
    signed_in["version"] = json!("<version>");
    golden("session-signed-in", &signed_in);
}
