//! Edges of the parent's API that the first full mutation sweep found untested
//! (`docs/OPEN-FINDINGS.md` O109): routines, the idempotency key's length, and the note a grant
//! carries when bedtime will cut it short.
//!
//! Its own test binary because every phase saves `config.json`, which goes wherever
//! `NESTWATCH_DATA_DIR` points, and that is process-wide. One test with ordered phases rather than
//! several, for the reason `curfew_extend.rs` gives: `#[tokio::test]` functions in one binary run
//! concurrently and would race over the same directory.

use axum::http::StatusCode;
use chrono::Duration;
use serde_json::{Value, json};
use tower::ServiceExt;

use nestwatch::rules::{EnforceAction, Rules};

mod common;
use common::{
    PASSWORD, ScratchDir, app_with, body_json, get, login, post_json, state_with, test_config,
};

fn rules(daily_budget_mins: u32) -> Value {
    serde_json::to_value(Rules {
        enabled: true,
        daily_budget_mins,
        budget_action: EnforceAction::Lock,
        ..Default::default()
    })
    .unwrap()
}

async fn routines(app: &axum::Router, cookie: &str) -> Vec<Value> {
    let res = get(app, "/api/routines", Some(cookie)).await;
    assert_eq!(res.status(), StatusCode::OK);
    body_json(res).await.as_array().cloned().expect("a list")
}

async fn save_routine(app: &axum::Router, cookie: &str, body: Value) -> StatusCode {
    post_json(app, "/api/routines", Some(cookie), body)
        .await
        .status()
}

/// A grant as the parent, with an `Idempotency-Key` of `len` characters.
async fn grant_with_key(app: &axum::Router, cookie: &str, len: usize) -> StatusCode {
    app.clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/extra-time")
                .header(axum::http::header::COOKIE, cookie)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "k".repeat(len))
                .body(axum::body::Body::from(json!({ "minutes": 5 }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn the_parents_api_holds_at_its_edges() {
    let tmp = ScratchDir::new("parent-api-edges");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let app = app_with(state_with(test_config()));
    let cookie = login(&app, PASSWORD).await.unwrap();

    // ---- Routines: saved, listed, replaced by name, deleted by name ---------------------------
    // `list_routines` could answer with an empty list, `save_routine` could find the routine to
    // replace by `!=` and so push a duplicate, and `delete_routine` could keep only the routine
    // it was asked to remove — all three green before this.
    let save = |name: &str, minutes: u32| json!({ "name": name, "rules": rules(minutes) });
    assert_eq!(
        save_routine(&app, &cookie, save("Homework", 60)).await,
        StatusCode::OK
    );
    let listed = routines(&app, &cookie).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["name"], "Homework");

    // The list carries each routine's schedule, not its rules, so the replacement is told apart by
    // the schedule it brings.
    let with_schedule = json!({
        "name": "Homework",
        "rules": rules(90),
        "schedule": [{ "start": "16:00", "end": "18:00" }],
    });
    assert_eq!(
        save_routine(&app, &cookie, with_schedule).await,
        StatusCode::OK
    );
    let listed = routines(&app, &cookie).await;
    assert_eq!(
        listed.len(),
        1,
        "saving under the same name replaces it: {listed:?}"
    );
    assert_eq!(listed[0]["schedule"][0]["start"], "16:00");

    assert_eq!(
        save_routine(&app, &cookie, save("Weekend", 180)).await,
        StatusCode::OK
    );
    let res = post_json(
        &app,
        "/api/routines/Homework/delete",
        Some(&cookie),
        json!({}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let names: Vec<Value> = routines(&app, &cookie)
        .await
        .into_iter()
        .map(|r| r["name"].clone())
        .collect();
    assert_eq!(
        names,
        vec![json!("Weekend")],
        "only the routine named is deleted"
    );

    // ---- A routine's name and schedule, at and past their limits ------------------------------
    // The name check is `is_empty() || longer than the limit`; turned into `&&` it lets an empty
    // name through, and a bound one too tight refused a name of exactly the limit.
    let max_name = nestwatch::config::MAX_ROUTINE_NAME;
    assert_eq!(
        save_routine(&app, &cookie, save("   ", 60)).await,
        StatusCode::BAD_REQUEST,
        "a blank name"
    );
    assert_eq!(
        save_routine(&app, &cookie, save(&"n".repeat(max_name), 60)).await,
        StatusCode::OK
    );
    assert_eq!(
        save_routine(&app, &cookie, save(&"n".repeat(max_name + 1), 60)).await,
        StatusCode::BAD_REQUEST
    );
    let windows = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|i| json!({ "start": format!("{:02}:00", i), "end": format!("{:02}:30", i) }))
            .collect()
    };
    let max_windows = nestwatch::config::MAX_SCHEDULE_WINDOWS;
    let scheduled =
        |n: usize| json!({ "name": "Scheduled", "rules": rules(60), "schedule": windows(n) });
    assert_eq!(
        save_routine(&app, &cookie, scheduled(max_windows)).await,
        StatusCode::OK
    );
    assert_eq!(
        save_routine(&app, &cookie, scheduled(max_windows + 1)).await,
        StatusCode::BAD_REQUEST
    );

    // ---- The idempotency key, at and past its length ------------------------------------------
    let max_key = nestwatch::idempotency::MAX_KEY_LEN;
    assert_eq!(grant_with_key(&app, &cookie, max_key).await, StatusCode::OK);
    assert_eq!(
        grant_with_key(&app, &cookie, max_key + 1).await,
        StatusCode::BAD_REQUEST
    );

    // ---- The note on a grant when bedtime will cut it short ------------------------------------
    // `grant_shadowed_by_curfew` could return nothing, an empty string or any string, and its
    // `mins == 0` test could be inverted, and no test noticed: none granted time near a bedtime.
    let now = nestwatch::clock::now();
    let hm = |t: chrono::DateTime<chrono::FixedOffset>| t.format("%H:%M").to_string();
    let curfew = |start: chrono::DateTime<chrono::FixedOffset>| json!({ "enabled": true, "start": hm(start), "end": hm(now + Duration::hours(2)), "warn_secs": 30 });
    let grant_note = || async {
        let res = post_json(
            &app,
            "/api/extra-time",
            Some(&cookie),
            json!({ "minutes": 30 }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        body_json(res).await["curfew_note"].clone()
    };

    let res = post_json(
        &app,
        "/api/curfew",
        Some(&cookie),
        curfew(now - Duration::hours(2)),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let note = grant_note().await;
    assert!(
        note.as_str()
            .is_some_and(|n| n.starts_with("Bedtime is in force now")),
        "granting during bedtime must say the PC will still shut down: {note}"
    );

    let res = post_json(
        &app,
        "/api/curfew",
        Some(&cookie),
        curfew(now + Duration::minutes(10)),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let note = grant_note().await;
    assert!(
        note.as_str()
            .is_some_and(|n| n.starts_with("Bedtime starts in")),
        "a grant that runs into bedtime must say how much of it is usable: {note}"
    );
}
