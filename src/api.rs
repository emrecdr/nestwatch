//! The `/api/*` handlers. Each one offloads its blocking OS call to a `spawn_blocking`
//! worker so the async runtime stays responsive, then maps the result into a response.
//! All routes here sit behind the `require_auth` middleware.

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

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
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("worker task failed: {e}")))?
        .map_err(AppError::from)
}

/// `GET /api/screenshot` → PNG image of the primary monitor.
pub async fn screenshot(State(state): State<AppState>) -> Result<Response, AppError> {
    let png = blocking(state.control.clone(), |c| c.screenshot_png()).await?;
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
    Ok(Json(json!({ "ok": true, "pid": pid })))
}

/// `POST /api/shutdown` → begin machine shutdown (short delay so the response is sent).
pub async fn shutdown(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    blocking(state.control.clone(), |c| {
        c.shutdown(5, Some("Shutting down (remote request)".into()))
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/curfew` → the current curfew settings.
pub async fn get_curfew(State(state): State<AppState>) -> Json<Curfew> {
    let curfew = state
        .curfew
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    Json(curfew)
}

/// `POST /api/curfew` → validate, persist, and hot-apply new curfew settings.
pub async fn set_curfew(
    State(state): State<AppState>,
    Json(new_curfew): Json<Curfew>,
) -> Result<Json<Value>, AppError> {
    new_curfew.validate().map_err(AppError::BadRequest)?;

    // Persist the whole config (curfew changes; port/password are unchanged). File I/O runs
    // off the async runtime.
    let persisted = Config {
        port: state.config.port,
        password_hash: state.config.password_hash.clone(),
        curfew: new_curfew.clone(),
    };
    tokio::task::spawn_blocking(move || persisted.save())
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("save task failed: {e}")))?
        .map_err(AppError::Internal)?;

    *state.curfew.write().unwrap_or_else(|p| p.into_inner()) = new_curfew;
    Ok(Json(json!({ "ok": true })))
}
