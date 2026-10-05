//! A settings change that could not be saved must not be kept, so that saving it again saves it.
//!
//! Its own test binary for the same reason as `rules_persist.rs`: it points `NESTWATCH_DATA_DIR` at
//! a scratch directory, and an environment variable is process-wide.
//!
//! The defect this pins was reproduced against `260f4d8` before it was fixed. `try_update_config`
//! applied the change in memory, failed to persist it, answered 500 — and kept the change. The
//! parent tapped Save again; the "changed nothing" comparison, made against memory that already
//! held the change, said nothing had changed, and the retry answered 200 without writing. Memory
//! said 90 minutes, `config.json` said 60, and the next restart put 60 back with nobody told.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt;

use nestwatch::config::data_paths;

mod common;
use common::{PASSWORD, ScratchDir, app_with, login, state_with, test_config};

async fn save_daily_limit(app: &axum::Router, cookie: &str, minutes: u32) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/rules")
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "daily_budget_mins": minutes,
                        "blocklist": [],
                        "app_limits": {},
                        "budget_action": "lock",
                        "warn_secs": 30
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn limit_on_disk() -> u64 {
    let raw = std::fs::read_to_string(data_paths().config).expect("config.json is readable");
    let saved: serde_json::Value = serde_json::from_str(&raw).unwrap();
    saved["rules"]["daily_budget_mins"].as_u64().unwrap()
}

#[tokio::test]
async fn a_change_that_could_not_be_saved_is_not_kept_and_saving_again_saves_it() {
    let tmp = ScratchDir::new("save-failure");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let state = state_with(test_config());
    let config = state.config.clone();
    let app = app_with(state);
    let cookie = login(&app, PASSWORD).await.unwrap();

    assert_eq!(save_daily_limit(&app, &cookie, 60).await, StatusCode::OK);
    assert_eq!(limit_on_disk(), 60);

    // A directory where `config.json` belongs: the atomic rename onto it fails on every platform
    // this builds for, which is the shape of a real failure (a full disk, a file held open by a
    // scanner) without depending on permissions a CI runner may not honour.
    let path = data_paths().config;
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();

    assert_eq!(
        save_daily_limit(&app, &cookie, 90).await,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a save that did not reach disk must not report success"
    );
    assert_eq!(
        nestwatch::state::recover_read(&config)
            .rules
            .daily_budget_mins,
        60,
        "the parent was told the change failed, so it must not be in force either — memory has to \
         agree with the file a restart will read"
    );

    // The disk recovers; the parent taps Save again.
    std::fs::remove_dir(&path).unwrap();
    assert_eq!(save_daily_limit(&app, &cookie, 90).await, StatusCode::OK);
    assert_eq!(
        limit_on_disk(),
        90,
        "the retry answered 200, so the change must be in config.json, not only in memory"
    );
    assert_eq!(
        nestwatch::state::recover_read(&config)
            .rules
            .daily_budget_mins,
        90
    );
}
