//! Errors go back to the browser the way the website's API sends them, `{ statusCode, message }`,
//! so the website shows the message as it would its own.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, AppError>;

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = json!({ "statusCode": self.status.as_u16(), "message": self.message });
        (self.status, Json(body)).into_response()
    }
}

// A broken database or a bad row is the host's fault, not the browser's. The details go to
// the host's log, not to whoever asked.
impl From<rusqlite::Error> for AppError {
    fn from(error: rusqlite::Error) -> Self {
        tracing::error!("database: {error}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The host had a problem with its data",
        )
    }
}

impl From<serde_json::Error> for AppError {
    fn from(error: serde_json::Error) -> Self {
        tracing::error!("stored data: {error}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The host had a problem with its data",
        )
    }
}
