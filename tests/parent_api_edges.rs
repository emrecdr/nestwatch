//! Edges of the parent's API that the first full mutation sweep found untested
//! (`docs/OPEN-FINDINGS.md` O109): routines, and the note a grant carries when bedtime will cut it
//! short. (The idempotency key's length, the third, is tested beside its other bound in
//! `earned_grant.rs`.)
//!
//! Its own test binary because every phase saves `config.json`, which goes wherever
//! `NESTWATCH_DATA_DIR` points, and that is process-wide. One test with ordered phases rather than
//! several, for the reason `curfew_extend.rs` gives: `#[tokio::test]` functions in one binary run
//! concurrently and would race over the same directory.

use axum::http::StatusCode;
use chrono::Duration;
use serde_json::{Value, json};

use nestwatch::rules::Rules;

mod common;
use common::{
    PASSWORD, ScratchDir, app_with, login, post_json, send_json, state_with, test_config,
};

async fn routines(app: &axum::Router, cookie: &str) -> Vec<Value> {
    let (status, body) = send_json(app, cookie, "GET", "/api/routines", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    body.as_array().cloned().expect("a list")
}

async fn save_routine(app: &axum::Router, cookie: &str, body: Value) -> StatusCode {
    post_json(app, "/api/routines", Some(cookie), body)
        .await
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
    let save = |name: &str| json!({ "name": name, "rules": Rules::default() });
    assert_eq!(
        save_routine(&app, &cookie, save("Homework")).await,
        StatusCode::OK
    );
    let listed = routines(&app, &cookie).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["name"], "Homework");

    // The list carries each routine's schedule, not its rules, so the replacement is told apart by
    // the schedule it brings.
    let with_schedule = json!({
        "name": "Homework",
        "rules": Rules::default(),
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
        save_routine(&app, &cookie, save("Weekend")).await,
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
        save_routine(&app, &cookie, save("   ")).await,
        StatusCode::BAD_REQUEST,
        "a blank name"
    );
    assert_eq!(
        save_routine(&app, &cookie, save(&"n".repeat(max_name))).await,
        StatusCode::OK
    );
    assert_eq!(
        save_routine(&app, &cookie, save(&"n".repeat(max_name + 1))).await,
        StatusCode::BAD_REQUEST
    );
    let windows = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|i| json!({ "start": format!("{:02}:00", i), "end": format!("{:02}:30", i) }))
            .collect()
    };
    let max_windows = nestwatch::config::MAX_SCHEDULE_WINDOWS;
    let scheduled = |n: usize| json!({ "name": "Scheduled", "rules": Rules::default(), "schedule": windows(n) });
    assert_eq!(
        save_routine(&app, &cookie, scheduled(max_windows)).await,
        StatusCode::OK
    );
    assert_eq!(
        save_routine(&app, &cookie, scheduled(max_windows + 1)).await,
        StatusCode::BAD_REQUEST
    );

    // ---- The note on a grant when bedtime will cut it short ------------------------------------
    // `grant_shadowed_by_curfew` could return nothing, an empty string or any string, and its
    // `mins == 0` test could be inverted, and no test noticed: none granted time near a bedtime.
    let now = nestwatch::clock::now();
    let hm = |t: chrono::DateTime<chrono::FixedOffset>| t.format("%H:%M").to_string();
    for (start, note_starts, why) in [
        (
            now - Duration::hours(2),
            "Bedtime is in force now",
            "granting during bedtime must say the PC will still shut down",
        ),
        (
            now + Duration::minutes(10),
            "Bedtime starts in",
            "a grant that runs into bedtime must say how much of it is usable",
        ),
    ] {
        let curfew = json!({ "enabled": true, "start": hm(start), "end": hm(now + Duration::hours(2)), "warn_secs": 30 });
        let (status, _) = send_json(&app, &cookie, "POST", "/api/curfew", curfew).await;
        assert_eq!(status, StatusCode::OK);
        let grant = json!({ "minutes": 30 });
        let (status, body) = send_json(&app, &cookie, "POST", "/api/extra-time", grant).await;
        assert_eq!(status, StatusCode::OK);
        let note = &body["curfew_note"];
        assert!(
            note.as_str().is_some_and(|n| n.starts_with(note_starts)),
            "{why}: {note}"
        );
    }
}
