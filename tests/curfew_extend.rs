//! `POST /api/curfew/extend` — the control a parent reaches for when they want to allow a late
//! finish, end to end.
//!
//! Its own test binary so the `NESTWATCH_DATA_DIR` override is isolated, and one test with ordered
//! phases rather than four, because that override is process-global and `#[tokio::test]` functions
//! in one binary run concurrently by default — four of them would race over the same directory.
//!
//! # Why this file exists
//!
//! The endpoint shipped with no coverage at all. `curfew.rs` unit-tests the pure question it asks
//! (`is_active_at` honouring the extension, exhaustively, including the midnight wrap), and that
//! is the part that was never in doubt. Everything *around* the decision was untested: whether the
//! instant is persisted, whether a second press adds to the first or replaces it, whether saving
//! the curfew form afterwards keeps it.
//!
//! **Phase 3 is the one that matters most.** `api::set_curfew` carries `extra_until` across a save
//! by hand, because the dashboard's curfew form does not send the field and assigning the posted
//! struct wholesale would silently revoke an extension granted minutes earlier. Deleting that one
//! line left the entire suite green — 497 tests, no failures — while re-creating the exact
//! complaint this feature was built to answer: a parent grants more time, the PC shuts down anyway,
//! and the dashboard reports success throughout. The changelog promises in so many words that an
//! extension "is not undone by saving the curfew form". This is what makes that a fact rather than
//! an intention.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt;

use nestwatch::config::data_paths;
use nestwatch::rules::{EnforceAction, Rules, Usage};

mod common;
use common::{PASSWORD, ScratchDir, app_with, login, state_with, test_config};

/// `POST` `body` to `uri` as the logged-in parent, returning the status and decoded JSON.
async fn post(
    app: &axum::Router,
    uri: &str,
    cookie: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn an_extension_stacks_persists_and_survives_saving_the_curfew_form() {
    let tmp = ScratchDir::new("curfew-extend");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let mut cfg = test_config();
    // A real screen-time budget, generous and unspent, so the phases below can tell "there is
    // nothing to warn about" apart from "the warning is missing".
    cfg.rules = Rules {
        enabled: true,
        daily_budget_mins: 600,
        budget_action: EnforceAction::Lock,
        ..Default::default()
    };
    cfg.curfew.enabled = true;
    cfg.curfew.start = "22:00".into();
    cfg.curfew.end = "07:00".into();

    let state = state_with(cfg);
    let config_handle = state.config.clone();
    let app = app_with(state);
    let cookie = login(&app, PASSWORD).await.unwrap();

    let stored = || {
        nestwatch::state::recover_read(&config_handle)
            .curfew
            .extra_until
    };
    assert!(stored().is_none(), "no extension before one is granted");

    // ---- Phase 1: granting one records an instant -------------------------------------------
    let (status, body) = post(&app, "/api/curfew/extend", &cookie, json!({"minutes": 30})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    let first = stored().expect("the extension must be recorded, not just reported");
    assert!(
        body["until"].as_str().is_some_and(|s| s.contains(':')),
        "the parent is told the time bedtime now falls at: {body}"
    );

    // ---- Phase 2: a second press adds to the first, rather than replacing it -----------------
    let (status, _) = post(&app, "/api/curfew/extend", &cookie, json!({"minutes": 30})).await;
    assert_eq!(status, StatusCode::OK);
    let second = stored().expect("still recorded");
    assert_eq!(
        (second - first).num_minutes(),
        30,
        "pressing +30 twice must give an hour. Extending from `now` instead of from the running \
         extension would make the second press swallow the first, so a parent who wanted an hour \
         would get half of it and no indication why."
    );

    // ---- Phase 3: saving the curfew form must not revoke it ----------------------------------
    // Exactly what the dashboard sends: the settings, and no `extra_until` — it is transient
    // state, not a setting, so it arrives as `None` and must be carried across by the handler.
    let (status, _) = post(
        &app,
        "/api/curfew",
        &cookie,
        json!({"enabled": true, "start": "22:00", "end": "07:00", "warn_secs": 30}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        stored(),
        Some(second),
        "saving the curfew form silently revoked tonight's extension. A parent who granted more \
         time and then touched anything on the Curfew card would have the PC shut down anyway, \
         having been told twice that it would not."
    );

    // ---- Phase 4: the same bound the screen-time grants use ----------------------------------
    let (status, _) = post(
        &app,
        "/api/curfew/extend",
        &cookie,
        json!({"minutes": 10_000}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an absurd extension must be refused with a message rather than stored"
    );
    assert_eq!(
        stored(),
        Some(second),
        "a refused extension must leave the running one exactly as it was"
    );

    // ---- Phase 5: the mirror of the curfew note, on the button built to answer it -------------
    // Nothing so far has produced a budget note: the configured budget is generous and the tally
    // is empty, so bedtime really was the only thing standing in the child's way. Spend it, and
    // the extension becomes a promise the screen-time limit will not keep.
    let (_, before) = post(&app, "/api/curfew/extend", &cookie, json!({"minutes": 30})).await;
    assert!(
        before["budget_note"].is_null(),
        "with screen time to spare there is nothing to warn about: {before}"
    );

    let spent = Usage {
        day: Some(nestwatch::config::today()),
        total_secs: 999 * 60,
        ..Default::default()
    };
    std::fs::write(
        data_paths().dir.join("usage_state.json"),
        serde_json::to_string(&spent).unwrap(),
    )
    .unwrap();

    let (status, body) = post(&app, "/api/curfew/extend", &cookie, json!({"minutes": 30})).await;
    assert_eq!(status, StatusCode::OK, "the extension still lands");
    let note = body["budget_note"]
        .as_str()
        .unwrap_or_else(|| panic!("no budget note while screen time is spent: {body}"));
    assert!(
        note.contains("lock"),
        "the note must say what will actually happen, using this install's own action: {note}"
    );
    assert!(
        note.contains("Add bonus time today"),
        "and point at the control that fixes it, by the name it has on the page: {note}"
    );
}
