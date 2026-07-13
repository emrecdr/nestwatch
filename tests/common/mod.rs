//! Shared test plumbing for the integration-test binaries. Included via `mod common;`.
//!
//! Each binary uses only a subset, so `dead_code` is allowed module-wide.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use nestwatch::audit::AuditLog;
use nestwatch::auth::hash_password;
use nestwatch::config::Config;
use nestwatch::control::FakeControl;
use nestwatch::server::build_router;
use nestwatch::state::AppState;
use nestwatch::timecode::TimeCodes;
use nestwatch::timereq::TimeRequests;
use nestwatch::usage::UsageLog;

pub const PASSWORD: &str = "test-password";

/// A default [`Config`] carrying the test password.
pub fn test_config() -> Config {
    Config {
        port: 8443,
        password_hash: hash_password(PASSWORD).unwrap(),
        ..Default::default()
    }
}

/// [`AppState`] from `config` with every on-disk log disabled. (`config.save()` still writes to
/// the data dir, so persistence tests redirect it via `NESTWATCH_DATA_DIR` before calling.)
pub fn state_with(config: Config) -> AppState {
    let mut state = AppState::new(Arc::new(FakeControl::new()), config);
    state.audit = Arc::new(AuditLog::disabled());
    state.usage = Arc::new(UsageLog::disabled());
    state.time_requests = Arc::new(TimeRequests::disabled());
    state.time_codes = Arc::new(TimeCodes::disabled());
    state
}

/// Disabled-log state carrying the test password (no data dir needed).
pub fn test_state() -> AppState {
    state_with(test_config())
}

/// Wrap a state in the router with a mock loopback peer, so the LAN-scope gate admits the
/// `oneshot` requests (which carry no real socket).
pub fn app_with(state: AppState) -> Router {
    build_router(state).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))))
}

/// The common case: a router over a fresh disabled-log state.
pub fn test_app() -> Router {
    app_with(test_state())
}

/// `POST /login`, returning the session cookie (`name=value`) on success, `None` otherwise.
pub async fn login(app: &Router, password: &str) -> Option<String> {
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

/// Deserialize a response body as JSON.
pub async fn body_json(res: axum::response::Response) -> Value {
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
