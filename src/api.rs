//! The `/api/*` handlers. Each one offloads its blocking OS call to a `spawn_blocking`
//! worker so the async runtime stays responsive, then maps the result into a response.
//! All routes here sit behind the `require_auth` middleware.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

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

/// `GET /api/curfew` → the current curfew settings.
pub async fn get_curfew(State(state): State<AppState>) -> Json<Curfew> {
    Json(crate::state::recover_read(&state.curfew).clone())
}

/// `POST /api/curfew` → validate, persist, and hot-apply new curfew settings.
pub async fn set_curfew(
    State(state): State<AppState>,
    Json(new_curfew): Json<Curfew>,
) -> Result<Json<Value>, AppError> {
    new_curfew.validate().map_err(AppError::BadRequest)?;

    // Persist the whole config (only curfew changes; port/password carry over). Cloning the
    // existing config picks up any future fields automatically. File I/O runs off the runtime.
    let mut persisted = (*state.config).clone();
    persisted.curfew = new_curfew.clone();
    tokio::task::spawn_blocking(move || persisted.save())
        .await?
        .map_err(AppError::Internal)?;

    state.audit.record(
        "curfew_change",
        json!({ "enabled": new_curfew.enabled, "start": new_curfew.start, "end": new_curfew.end }),
    );
    *crate::state::recover_write(&state.curfew) = new_curfew;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/audit` → the most recent security-audit events (newest first), so the parent can
/// see logins and their source IP. Read-only; behind `require_auth` like the rest of `/api`.
pub async fn audit(State(state): State<AppState>) -> Result<Json<Vec<Value>>, AppError> {
    let audit = state.audit.clone();
    let events = tokio::task::spawn_blocking(move || audit.recent(200)).await?;
    Ok(Json(events))
}
