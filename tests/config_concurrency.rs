//! Concurrent config writes must not lose one another.
//!
//! `api::update_config` cannot hold the config `RwLock` across its `.await` — a std guard over an
//! await point makes the future `!Send`. So the guard is dropped between mutating and persisting,
//! and without `config_save_lock` two handlers can interleave as mutate-A, mutate-B, save-B,
//! save-A. Both changes are in memory, but the *older* snapshot lands on disk last and silently
//! reverts the newer one at the next restart.
//!
//! This is reachable from one parent on one phone: approving a time request while a rules save is
//! still in flight. `redeem_code` writes config too and needs no login at all.
//!
//! Its own test binary so the `NESTWATCH_DATA_DIR` override is isolated from the other
//! integration tests, matching `rules_persist.rs` and `password_change.rs`.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt;

use nestwatch::config::{Config, data_paths};

mod common;
use common::{PASSWORD, app_with, login, state_with, test_config};

/// Fire many `/api/rules` and `/api/extra-time` writes concurrently, then require that the file
/// on disk is exactly the config the service is running on. Any lost update shows up as a
/// mismatch between the two, which is precisely the bug: memory right, disk stale.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_config_writes_all_reach_disk() {
    let tmp = std::env::temp_dir().join(format!("nw-conc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", &tmp) };

    let state = state_with(test_config());
    let config_handle = state.config.clone();
    let app = app_with(state);
    let cookie = login(&app, PASSWORD).await.unwrap();

    // Two different fields, so a lost update cannot hide behind an identical value: whichever
    // save lands last must still carry the other's change.
    let mut tasks = Vec::new();
    for i in 0..24u32 {
        let (app, cookie) = (app.clone(), cookie.clone());
        tasks.push(tokio::spawn(async move {
            let (uri, body) = if i % 2 == 0 {
                (
                    "/api/rules",
                    json!({ "daily_budget_mins": 60 + i }).to_string(),
                )
            } else {
                ("/api/extra-time", json!({ "minutes": 5 }).to_string())
            };
            let res = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header(header::COOKIE, &cookie)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{uri} rejected");
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    // The file must parse. Sharing one temp path used to publish a blend of two writers here.
    let raw = std::fs::read_to_string(data_paths().config).expect("config.json readable");
    let on_disk: Config =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("config.json is not valid JSON: {e}"));

    // And it must be the config the service is actually running on -- not an older snapshot.
    let in_memory = { nestwatch::state::recover_read(&config_handle).clone() };
    assert_eq!(
        on_disk.rules.daily_budget_mins, in_memory.rules.daily_budget_mins,
        "disk holds a stale budget: a concurrent save overwrote a newer one"
    );
    assert_eq!(
        on_disk.extra.for_day(nestwatch::config::today()),
        in_memory.extra.for_day(nestwatch::config::today()),
        "disk lost granted minutes: a concurrent save overwrote a newer one"
    );

    // Every grant must have survived; 12 odd indices x 5 minutes.
    assert_eq!(
        on_disk.extra.for_day(nestwatch::config::today()),
        60,
        "some grants were lost between memory and disk"
    );

    // No writer may leave its scratch file behind.
    let leftovers: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
