//! HTTP-level integration tests. They drive the real router (via `tower`'s `oneshot`)
//! backed by `FakeControl`, so they run on any OS with no real side effects — this is the
//! payoff of the `SystemControl` abstraction.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt; // for `oneshot`

use nestwatch::auth::hash_password;
use nestwatch::config::Config;
use nestwatch::control::FakeControl;
use nestwatch::server::build_router;
use nestwatch::state::AppState;

const PASSWORD: &str = "test-password";

fn test_state() -> AppState {
    AppState::new(
        Arc::new(FakeControl::new()),
        Config {
            port: 8443,
            password_hash: hash_password(PASSWORD).unwrap(),
            curfew: Default::default(),
        },
    )
}

/// POST /login and return the session cookie (`name=value`) on success.
async fn login(app: &axum::Router, password: &str) -> Option<String> {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "password": password }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    if res.status() != StatusCode::OK {
        return None;
    }
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)?
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    Some(cookie)
}

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn api_requires_auth() {
    let app = build_router(test_state());
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/processes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_password_is_rejected() {
    let app = build_router(test_state());
    assert!(login(&app, "not-the-password").await.is_none());
}

#[tokio::test]
async fn session_endpoint_reflects_auth_state() {
    let app = build_router(test_state());

    // Anonymous.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(res).await["authenticated"], json!(false));

    // Authenticated.
    let cookie = login(&app, PASSWORD).await.expect("login should succeed");
    let res = app
        .oneshot(
            Request::builder()
                .uri("/session")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(res).await["authenticated"], json!(true));
}

#[tokio::test]
async fn screenshot_returns_png() {
    let app = build_router(test_state());
    let cookie = login(&app, PASSWORD).await.unwrap();

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/screenshot")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/png"
    );
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[1..4], b"PNG", "PNG magic bytes present");
}

#[tokio::test]
async fn curfew_get_and_validation() {
    let app = build_router(test_state());
    let cookie = login(&app, PASSWORD).await.unwrap();

    // GET returns the default (disabled) curfew.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/curfew")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["enabled"], json!(false));

    // POST with a malformed time is rejected (400) before anything is persisted.
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/curfew")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"enabled": true, "start": "25:99", "end": "07:00", "warn_secs": 60})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn processes_list_then_kill() {
    let app = build_router(test_state());
    let cookie = login(&app, PASSWORD).await.unwrap();

    // List includes a known fake process.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/processes")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let list = body_json(res).await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "notepad.exe")
    );

    // Kill an existing PID → 200.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/processes/1005/kill")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Kill a non-existent PID → 404.
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/processes/999999/kill")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
