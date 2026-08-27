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

/// A temporary directory that removes itself however the test ends.
///
/// Manual cleanup on the last line of a test body only runs when the test *reaches* that line. An
/// assertion failure is a panic, so the run that most wants its scratch data kept is also the only
/// one that leaks it, and the next run starts against a directory it believes it created fresh.
///
/// This was the argument already written above `AuditFileApp`, which grew a `Drop` for exactly
/// this reason — but the reasoning stayed welded to the audit log, so the same hand-rolled
/// `temp_dir().join(...)` / `remove_dir_all` / `create_dir_all` dance was written out again for
/// the screen-time log, the time-request log and the time-code queue, each ending in the manual
/// cleanup line the guard exists to make unnecessary. The directory is the thing that needs the
/// guard; which log happens to be written inside it is not part of the problem.
///
/// **Bind it to a name, never to `_`.** `let _ = ScratchDir::new(..)` drops the value immediately
/// and deletes the directory before the test uses it; `let _dir = ..` keeps it to end of scope,
/// which is the whole point.
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// `tag` separates tests running concurrently inside one binary; the process id already
    /// separates the binaries from each other.
    pub fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("nw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    /// A path inside the directory. The file need not exist.
    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A router whose audit log writes to a real file, with the path to read it back.
///
/// Every other helper here disables the on-disk logs; a handful of tests need the opposite,
/// because the property under test is what actually lands in `audit.jsonl`.
pub struct AuditFileApp {
    pub app: Router,
    /// The `audit.jsonl` this router appends to.
    pub path: PathBuf,
    /// Held, not read: this is what deletes the directory when the test ends.
    _dir: ScratchDir,
}

/// `tag` names the scratch directory; see [`ScratchDir::new`] for how tests are kept apart.
pub fn app_with_audit_file(tag: &str) -> AuditFileApp {
    let dir = ScratchDir::new(tag);
    let path = dir.join("audit.jsonl");

    let mut state = test_state();
    state.audit = Arc::new(AuditLog::new(path.clone()));
    AuditFileApp {
        app: app_with(state),
        path,
        _dir: dir,
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
