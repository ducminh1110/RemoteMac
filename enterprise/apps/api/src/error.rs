//! One error type for the API, one JSON shape for clients:
//! `{"error": {"code": "...", "message": "...", "request_id": "..."}}`. Internal details are
//! logged (without secrets), never returned.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not signed in")]
    Unauthenticated,
    #[error("not allowed")]
    Forbidden,
    /// also used when a resource exists but the caller may not know it does
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
    #[error("too many requests")]
    RateLimited,
    #[error("the CSRF token is missing or wrong")]
    Csrf,
    #[error("{0}")]
    Unavailable(String),
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            ApiError::Unauthenticated => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden | ApiError::Csrf => StatusCode::FORBIDDEN,
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ApiError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            ApiError::Unauthenticated => "unauthenticated",
            ApiError::Forbidden => "forbidden",
            ApiError::NotFound => "not_found",
            ApiError::Conflict(_) => "conflict",
            ApiError::Invalid(_) => "invalid",
            ApiError::RateLimited => "rate_limited",
            ApiError::Csrf => "csrf",
            ApiError::Unavailable(_) => "unavailable",
            ApiError::Internal(_) => "internal",
        }
    }

    pub fn invalid(m: impl Into<String>) -> ApiError {
        ApiError::Invalid(m.into())
    }

    pub fn conflict(m: impl Into<String>) -> ApiError {
        ApiError::Conflict(m.into())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        if let sqlx::Error::RowNotFound = e {
            return ApiError::NotFound;
        }
        if let Some(db) = e.as_database_error() {
            // a unique or check constraint the request broke: a conflict, not a crash
            if db.is_unique_violation() {
                return ApiError::Conflict("that already exists".into());
            }
            if db.is_check_violation() || db.is_foreign_key_violation() {
                return ApiError::Invalid("the request does not fit the data rules".into());
            }
        }
        ApiError::Internal(e.into())
    }
}

tokio::task_local! {
    /// The request's id, for error bodies and logs (set by the request-id middleware).
    pub static REQUEST_ID: String;
}

pub fn request_id() -> String {
    REQUEST_ID.try_with(|r| r.clone()).unwrap_or_default()
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let rid = request_id();
        let message = match &self {
            ApiError::Internal(e) => {
                tracing::error!(request_id = %rid, error = %crate::redact::redact(&format!("{e:#}")), "internal error");
                "Something went wrong on our side. Try again; if it keeps happening, contact support with the request id.".to_string()
            }
            other => other.to_string(),
        };
        let body =
            json!({ "error": { "code": self.code(), "message": message, "request_id": rid } });
        (self.status(), axum::Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
