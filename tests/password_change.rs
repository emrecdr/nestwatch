//! Password-change end-to-end. In its own test binary so its `NESTWATCH_DATA_DIR` override
//! runs in a separate process and can't affect (or be affected by) the other integration tests.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt;

use nestwatch::audit::AuditLog;
use nestwatch::auth::hash_password;
use nestwatch::config::Config;
use nestwatch::control::FakeControl;
use nestwatch::server::build_router;
use nestwatch::state::AppState;

const PASSWORD: &str = "test-password";

async fn post_login(app: &axum::Router, password: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "password": password }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn password_change_end_to_end() {
    let tmp = std::env::temp_dir().join(format!("nw-pw-{}", std::process::id()));
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", &tmp) };

    let mut state = AppState::new(
        Arc::new(FakeControl::new()),
        Config {
            port: 8443,
            password_hash: hash_password(PASSWORD).unwrap(),
            curfew: Default::default(),
        },
    );
    state.audit = Arc::new(AuditLog::disabled());
    state.usage = Arc::new(nestwatch::usage::UsageLog::disabled());
    let app = build_router(state).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));

    // Log in with the original password.
    let res = post_login(&app, PASSWORD).await;
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

    // Change the password.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/password")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "current": PASSWORD, "new": "a-fresh-passphrase" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // The new password now works; the old one does not.
    assert_eq!(
        post_login(&app, "a-fresh-passphrase").await.status(),
        StatusCode::OK
    );
    assert_ne!(post_login(&app, PASSWORD).await.status(), StatusCode::OK);

    let _ = std::fs::remove_dir_all(&tmp);
}
