//! Laravel has no shared response envelope here (confirmed: every controller builds its JSON ad
//! hoc -- see docs/WORKFLOW.md and the plan). `AppError` is **not** a generic envelope for
//! business responses; it only covers the handful of places that genuinely do share one literal
//! shape in the real app: the auth middleware's `{"message": "..."}` 401/403s and validation
//! failures. Every handler that needs a different shape (`{"success":...}`, `{"ok":...}`,
//! `{"error":...}`, or a bare scalar/array) builds its own response directly instead of going
//! through this type.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub enum AppError {
    Internal(anyhow::Error),
    Unauthorized(String),
    Forbidden(String),
    Validation(String),
    NotFound(String),
}

impl AppError {
    pub fn internal(err: impl Into<anyhow::Error>) -> Self {
        AppError::Internal(err.into())
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        AppError::Internal(err.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Internal(err) => {
                tracing::error!(error = %err, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "Unexpected server error.".to_string())
            }
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg.clone()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            AppError::Validation(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg.clone()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
        };
        (status, Json(json!({ "message": message }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
