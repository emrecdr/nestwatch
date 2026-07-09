//! The `/api/*` handlers. Each one offloads its blocking OS call to a `spawn_blocking`
//! worker so the async runtime stays responsive, then maps the result into a response.
//! All routes here sit behind the `require_auth` middleware.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_sessions::Session;

use crate::timereq::{MAX_REQUEST_MINUTES, PendingRequest};

use crate::config::Config;
use crate::control::{ProcessInfo, SystemControl};
use crate::curfew::Curfew;
use crate::error::AppError;
use crate::state::AppState;

/// Run a blocking `SystemControl` call on the blocking thread pool.
async fn blocking<T, F>(control: std::sync::Arc<dyn SystemControl>, f: F) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce(&dyn SystemControl) -> Result<T, crate::control::ControlError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || f(control.as_ref()))
        .await?
        .map_err(AppError::from)
}

/// Offload a blocking closure (file I/O, password hashing, log reads) to the blocking pool.
/// Sibling of [`blocking`] for work that doesn't take a `SystemControl`; a `JoinError` maps to
/// `AppError` via its `From` impl.
async fn spawn<T, F>(f: F) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(AppError::from)
}

/// Mutate the single-source [`Config`] and persist it off the async runtime.
///
/// SAFETY/ORDERING: the std `RwLock` write guard is dropped at the end of the inner block —
/// BEFORE any `.await` — so it never crosses an await point (which would trip clippy's
/// `await_holding_lock` and make the future `!Send`). We apply `mutate`, clone the whole
/// `Config` out under the guard, release the lock, then save the owned snapshot on a blocking
/// thread. Callers should `validate()` before calling.
async fn update_config<F>(state: &AppState, mutate: F) -> Result<(), AppError>
where
    F: FnOnce(&mut Config),
{
    let snapshot = {
        let mut guard = crate::state::recover_write(&state.config);
        mutate(&mut guard);
        guard.clone()
    };
    spawn(move || snapshot.save())
        .await?
        .map_err(AppError::Internal)
}

/// `GET /api/screenshot` → PNG image of the primary monitor.
pub async fn screenshot(State(state): State<AppState>) -> Result<Response, AppError> {
    let png = blocking(state.control.clone(), |c| c.screenshot_png()).await?;
    state.audit.record("screenshot_taken", json!({}));
    Ok(([(header::CONTENT_TYPE, "image/png")], png).into_response())
}

/// `GET /api/processes` → JSON array of running processes.
pub async fn list_processes(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProcessInfo>>, AppError> {
    let list = blocking(state.control.clone(), |c| c.list_processes()).await?;
    Ok(Json(list))
}

/// `POST /api/processes/{pid}/kill` → terminate a process.
pub async fn kill_process(
    State(state): State<AppState>,
    Path(pid): Path<u32>,
) -> Result<Json<Value>, AppError> {
    blocking(state.control.clone(), move |c| c.kill_process(pid)).await?;
    state.audit.record("process_kill", json!({ "pid": pid }));
    Ok(Json(json!({ "ok": true, "pid": pid })))
}

/// `POST /api/shutdown` → begin machine shutdown (short delay so the response is sent).
pub async fn shutdown(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    blocking(state.control.clone(), |c| {
        c.shutdown(5, Some("Shutting down (remote request)".into()))
    })
    .await?;
    state.audit.record("shutdown_issued", json!({}));
    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/lock` → lock the screen (softer than shutdown; password to resume).
pub async fn lock(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    blocking(state.control.clone(), |c| c.lock_workstation()).await?;
    state.audit.record("lock_issued", json!({}));
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/curfew` → the current curfew settings.
pub async fn get_curfew(State(state): State<AppState>) -> Json<Curfew> {
    Json(crate::state::recover_read(&state.config).curfew.clone())
}

/// `POST /api/curfew` → validate, persist, and hot-apply new curfew settings.
pub async fn set_curfew(
    State(state): State<AppState>,
    Json(new_curfew): Json<Curfew>,
) -> Result<Json<Value>, AppError> {
    new_curfew.validate().map_err(AppError::BadRequest)?;
    let audit_fields =
        json!({ "enabled": new_curfew.enabled, "start": new_curfew.start, "end": new_curfew.end });
    update_config(&state, |c| c.curfew = new_curfew).await?;
    state.audit.record("curfew_change", audit_fields);
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/audit` → the most recent security-audit events (newest first), so the parent can
/// see logins and their source IP. Read-only; behind `require_auth` like the rest of `/api`.
pub async fn audit(State(state): State<AppState>) -> Result<Json<Vec<Value>>, AppError> {
    let audit = state.audit.clone();
    let events = spawn(move || audit.recent(200)).await?;
    Ok(Json(events))
}

/// `GET /api/usage` → the most recent usage-history events (newest first): daily screen-time,
/// sessions, and enforcement actions. Read-only; behind `require_auth`.
pub async fn usage(State(state): State<AppState>) -> Result<Json<Vec<Value>>, AppError> {
    let usage = state.usage.clone();
    let events = spawn(move || usage.recent(200)).await?;
    Ok(Json(events))
}

/// `GET /api/rules` → the current usage rules (budget, blocklist, per-app limits).
pub async fn get_rules(State(state): State<AppState>) -> Json<crate::rules::Rules> {
    Json(crate::state::recover_read(&state.config).rules.clone())
}

/// `POST /api/rules` → validate, persist, and hot-apply new usage rules.
pub async fn set_rules(
    State(state): State<AppState>,
    Json(new_rules): Json<crate::rules::Rules>,
) -> Result<Json<Value>, AppError> {
    new_rules.validate().map_err(AppError::BadRequest)?;
    let audit_fields = json!({
        "daily_budget_mins": new_rules.daily_budget_mins,
        "blocklist_count": new_rules.blocklist.len(),
        "app_limits_count": new_rules.app_limits.len(),
        "budget_action": new_rules.budget_action,
    });
    update_config(&state, |c| c.rules = new_rules).await?;
    state.audit.record("rules_change", audit_fields);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct TimeReqBody {
    minutes: u32,
    #[serde(default)]
    reason: String,
}

/// `POST /time-request` — the child asks for extra minutes. **Unauthenticated** (the child
/// isn't logged in) but LAN-gated (outer router → `require_lan_peer`) and per-IP rate-limited.
/// Returns only `{ok:true}` regardless of accept/reject, so it leaks nothing about the queue.
pub async fn time_request(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<TimeReqBody>,
) -> Result<Json<Value>, AppError> {
    let ip = peer.ip();
    state.time_req_limiter.count_and_check(ip)?;
    if body.minutes == 0 || body.minutes > MAX_REQUEST_MINUTES {
        return Err(AppError::BadRequest("minutes out of range".into()));
    }
    let requests = state.time_requests.clone();
    let accepted = spawn(move || requests.submit(body.minutes, &body.reason)).await?;
    state.audit.record(
        "time_request_submitted",
        json!({ "src_ip": ip, "minutes": body.minutes, "accepted": accepted.is_some() }),
    );
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/time-requests` → the pending requests, newest first (parent-facing).
pub async fn list_time_requests(
    State(state): State<AppState>,
) -> Result<Json<Vec<PendingRequest>>, AppError> {
    let requests = state.time_requests.clone();
    let pending = spawn(move || requests.pending()).await?;
    Ok(Json(pending))
}

/// `POST /api/time-requests/{id}/approve` → grant the requested minutes to today's budget.
pub async fn approve_time_request(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let requests = state.time_requests.clone();
    let resolved = spawn(move || requests.resolve(&id, true)).await?;
    let Some(req) = resolved else {
        return Err(AppError::BadRequest("no such pending request".into()));
    };

    // Add the minutes to today's grant (the reset-if-not-today rule lives in DailyGrant).
    let today = crate::config::today();
    let minutes = req.minutes;
    update_config(&state, |c| c.extra.add(today, minutes)).await?;

    state
        .audit
        .record("time_request_approved", json!({ "minutes": minutes }));
    state
        .usage
        .record("extra_time_granted", json!({ "minutes": minutes }));
    Ok(Json(json!({ "ok": true, "minutes": minutes })))
}

/// `POST /api/time-requests/{id}/deny` → reject a pending request.
pub async fn deny_time_request(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let requests = state.time_requests.clone();
    let resolved = spawn(move || requests.resolve(&id, false)).await?;
    if resolved.is_none() {
        return Err(AppError::BadRequest("no such pending request".into()));
    }
    state.audit.record("time_request_denied", json!({}));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct PasswordChange {
    current: String,
    new: String,
}

/// `POST /api/password` → verify the current password, then set a new one (Argon2id re-hash,
/// persisted). Lets the parent rotate the password without re-running `install`.
///
/// Session policy: the current session stays valid (its id is rotated defensively); other
/// sessions are not revoked — `MemoryStore` has no principal-scoped revocation and there is a
/// single parent, so global revocation buys nothing here.
pub async fn change_password(
    State(state): State<AppState>,
    session: Session,
    Json(body): Json<PasswordChange>,
) -> Result<Json<Value>, AppError> {
    if body.new.chars().count() < crate::auth::MIN_PASSWORD_LEN {
        return Err(AppError::BadRequest(format!(
            "new password must be at least {} characters",
            crate::auth::MIN_PASSWORD_LEN
        )));
    }

    // Verify the current password off the async runtime (Argon2 is memory-hard).
    let current_hash = crate::state::recover_read(&state.config)
        .password_hash
        .clone();
    let candidate = body.current;
    let ok = spawn(move || crate::auth::verify_password(&candidate, &current_hash)).await?;
    if !ok {
        state
            .audit
            .record("password_change_failed", json!({ "reason": "bad_current" }));
        return Err(AppError::Unauthorized);
    }

    // Hash the new password off the runtime, then persist via the single-source helper.
    let new_pw = body.new;
    let new_hash = spawn(move || crate::auth::hash_password(&new_pw))
        .await?
        .map_err(AppError::Internal)?;
    update_config(&state, |c| c.password_hash = new_hash).await?;

    // Rotate this session's id (defensive), keeping the parent logged in.
    session.cycle_id().await?;
    state.audit.record("password_changed", json!({}));
    Ok(Json(json!({ "ok": true })))
}
