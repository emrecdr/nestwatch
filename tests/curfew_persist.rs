//! A valid `POST /api/curfew` round-trip: it should update the in-memory state AND persist
//! to disk. This lives in its own test binary so its `NESTWATCH_DATA_DIR` override runs in a
//! separate process and can't affect (or be affected by) the other integration tests.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt;

use nestwatch::auth::hash_password;
use nestwatch::config::{Config, data_paths};
use nestwatch::control::FakeControl;
use nestwatch::server::build_router;
use nestwatch::state::AppState;

const PASSWORD: &str = "test-password";

#[tokio::test]
async fn valid_curfew_persists_and_updates_state() {
    // Redirect the data dir to a temp location so we never touch the real config.
    let tmp = std::env::temp_dir().join(format!("nw-curfew-{}", std::process::id()));
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", &tmp) };

    let state = AppState::new(
        Arc::new(FakeControl::new()),
        Config {
            port: 8443,
            password_hash: hash_password(PASSWORD).unwrap(),
            curfew: Default::default(),
        },
    );
    let config_handle = state.config.clone();
    // Mock loopback peer so the LAN-scope gate admits the oneshot request.
    let app = build_router(state).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));

    // Log in.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "password": PASSWORD }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    // POST a valid curfew.
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/curfew")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"enabled": true, "start": "21:00", "end": "06:30", "warn_secs": 30})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // In-memory state updated...
    assert!(
        nestwatch::state::recover_read(&config_handle)
            .curfew
            .enabled
    );
    assert_eq!(
        nestwatch::state::recover_read(&config_handle).curfew.start,
        "21:00"
    );

    // ...and persisted to disk.
    let saved = std::fs::read_to_string(data_paths().config).unwrap();
    assert!(saved.contains("21:00"), "curfew persisted to config.json");

    let _ = std::fs::remove_dir_all(&tmp);
}
