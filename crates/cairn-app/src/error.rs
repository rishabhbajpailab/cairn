use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use cairn_store::StoreError;
use serde_json::json;

/// `{ "error": { "code": "snake_case", "message": "…" } }`
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl ToString) -> Self {
        Self { status, code, message: message.to_string() }
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "missing or invalid token")
    }
    pub fn forbidden(scope: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", format!("token lacks `{scope}` scope"))
    }
    pub fn not_found(what: impl ToString) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", what)
    }
    pub fn bad_request(msg: impl ToString) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", msg)
    }
    pub fn not_implemented(what: &str) -> Self {
        Self::new(StatusCode::NOT_IMPLEMENTED, "not_implemented", format!("{what} (see docs/ROADMAP.md)"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": { "code": self.code, "message": self.message } }))).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(m) => Self::new(StatusCode::NOT_FOUND, "not_found", m),
            StoreError::Conflict(m) => Self::new(StatusCode::CONFLICT, "conflict", m),
            StoreError::Forbidden(m) => Self::new(StatusCode::FORBIDDEN, "forbidden_op", m),
            StoreError::InvalidQuery(m) => Self::new(StatusCode::BAD_REQUEST, "invalid_query", m),
            StoreError::Unimplemented(m) => Self::not_implemented(m),
            other => {
                tracing::error!(error = %other, "store error");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "store", "internal storage error")
            }
        }
    }
}
