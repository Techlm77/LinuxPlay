//! How a request fails: an HTTP status and the JSON body that explains it.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use linuxplay_proto::rendezvous::ApiError;
use tracing::error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl Failure {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    /// The device to reach is not running as a host right now.
    pub fn offline(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "offline", message)
    }

    pub fn rate_limited() -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Too many attempts. Wait a little and try again.")
    }

    /// Something went wrong on our side. The cause is logged, not shown.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        error!("internal error: {cause}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "The server had a problem. Try again shortly.")
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.code.into(), message: self.message })).into_response()
    }
}

pub type ApiResult<T> = Result<T, Failure>;
