//! One error type for the whole HTTP layer.
//!
//! Handlers return `Result<_, AppError>`; the mapping from a typed error to an HTTP
//! status code + JSON body happens here, in exactly one place, via `IntoResponse`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::control::ControlError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("authentication required")]
    Unauthorized,

    #[error("too many failed attempts, try again shortly")]
    TooManyAttempts,

    #[error("{0}")]
    BadRequest(String),

    /// An OS operation failed (screenshot, process list/kill, shutdown).
    #[error(transparent)]
    Control(#[from] ControlError),

    /// Anything unexpected. The detail is logged, never leaked to the client.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// A panicked/cancelled `spawn_blocking` worker maps to a generic 500 (detail is logged).
impl From<tokio::task::JoinError> for AppError {
    fn from(e: tokio::task::JoinError) -> Self {
        AppError::Internal(anyhow::anyhow!("blocking task failed: {e}"))
    }
}

/// Session-store failures (cookie read/write/flush) are internal errors, never client-facing.
impl From<tower_sessions::session::Error> for AppError {
    fn from(e: tower_sessions::session::Error) -> Self {
        AppError::Internal(anyhow::anyhow!(e))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::TooManyAttempts => (StatusCode::TOO_MANY_REQUESTS, self.to_string()),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Control(ControlError::ProcessNotFound(_)) => {
                (StatusCode::NOT_FOUND, self.to_string())
            }
            AppError::Control(err) => {
                // Log the OS detail; don't leak it to the client.
                tracing::error!(error = %err, "control operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "operation failed".to_string(),
                )
            }
            AppError::Internal(err) => {
                tracing::error!(error = ?err, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
