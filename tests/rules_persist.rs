//! Valid mutating round-trips (`POST /api/rules`, `POST /api/extra-time`) update in-memory state
//! AND persist to disk. Its own test binary so the `NESTWATCH_DATA_DIR` override is isolated from
//! the other integration tests (and from a second env-setting test running concurrently).

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt;

use nestwatch::config::data_paths;

mod common;
use common::{PASSWORD, app_with, login, state_with, test_config};

#[tokio::test]
async fn valid_rules_persist_and_update_state() {
    let tmp = std::env::temp_dir().join(format!("nw-rules-{}", std::process::id()));
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", &tmp) };

    let state = state_with(test_config());
    let config_handle = state.config.clone();
    let app = app_with(state);

    let cookie = login(&app, PASSWORD).await.unwrap();

    // POST valid rules.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/rules")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "daily_budget_mins": 90,
                        "blocklist": ["game.exe"],
                        "app_limits": { "chrome.exe": 60 },
                        "budget_action": "lock",
                        "warn_secs": 30
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // In-memory state updated... (scoped so the read guard never spans the await below)
    {
        let cfg = nestwatch::state::recover_read(&config_handle);
        assert_eq!(cfg.rules.daily_budget_mins, 90);
        assert_eq!(cfg.rules.blocklist, vec!["game.exe".to_string()]);
    }

    // ...and persisted to disk.
    let saved = std::fs::read_to_string(data_paths().config).unwrap();
    assert!(saved.contains("game.exe"), "rules persisted to config.json");

    // A parent bonus-time grant lands in today's DailyGrant, in memory and on disk.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extra-time")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "minutes": 30 }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    {
        let cfg = nestwatch::state::recover_read(&config_handle);
        assert_eq!(cfg.extra.for_day(nestwatch::config::today()), 30);
    }
    let saved = std::fs::read_to_string(data_paths().config).unwrap();
    assert!(saved.contains("\"minutes\": 30"), "grant persisted");

    // Save a routine (budget 15), then apply it → the live rules become the routine's rules.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/routines")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "name": "Homework", "rules": { "daily_budget_mins": 15 } }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/routines/Homework/apply")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    {
        let cfg = nestwatch::state::recover_read(&config_handle);
        assert_eq!(cfg.routines.len(), 1);
        assert_eq!(
            cfg.rules.daily_budget_mins, 15,
            "routine applied to live rules"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}
