//! Shared test plumbing for the integration-test binaries. Included via `mod common;`.
//!
//! Each binary uses only a subset, so `dead_code` is allowed module-wide.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
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
use nestwatch::screentime::ScreentimeLog;
use nestwatch::server::build_router;
use nestwatch::sessionstore::FileSessionStore;
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
    state.screentime = Arc::new(ScreentimeLog::disabled());
    // In-memory sessions, so the suite never writes sessions.json into a real data dir.
    state.sessions = FileSessionStore::ephemeral();
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

/// A router whose audit log writes to a real file, with the path to read it back.
///
/// Every other helper here disables the on-disk logs; a handful of tests need the opposite,
/// because the property under test is what actually lands in `audit.jsonl`.
///
/// The self-cleaning directory is why this is shared rather than left as three copies. Each copy
/// deleted its directory on the last line of the test body, so any of them that later grew an
/// early return — or simply tripped an assertion, which is a panic — would leak one silently, and
/// the next run would start against a directory it thought it had created fresh. Dropping the
/// guard cleans up on every path.
pub struct AuditFileApp {
    pub app: Router,
    /// The `audit.jsonl` this router appends to.
    pub path: PathBuf,
    dir: PathBuf,
}

impl Drop for AuditFileApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `tag` separates tests running concurrently inside one binary; the process id already separates
/// the binaries from each other.
pub fn app_with_audit_file(tag: &str) -> AuditFileApp {
    let dir = std::env::temp_dir().join(format!("nw-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("audit.jsonl");

    let mut state = test_state();
    state.audit = Arc::new(AuditLog::new(path.clone()));
    AuditFileApp {
        app: app_with(state),
        path,
        dir,
    }
}

/// `GET uri`, optionally carrying a session cookie.
pub async fn get(app: &Router, uri: &str, cookie: Option<&str>) -> axum::response::Response {
    let mut b = Request::builder().uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    app.clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// `POST uri` with a JSON body, optionally carrying a session cookie.
pub async fn post_json(
    app: &Router,
    uri: &str,
    cookie: Option<&str>,
    body: Value,
) -> axum::response::Response {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    app.clone()
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

/// `POST /login`, returning the session cookie (`name=value`) on success, `None` otherwise.
pub async fn login(app: &Router, password: &str) -> Option<String> {
    let res = post_json(app, "/login", None, json!({ "password": password })).await;
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
