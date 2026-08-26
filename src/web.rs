//! HTTP response helpers and the error shape.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

/// Every failure leaves in this shape — `{"error": "...", "detail": {...}}`.
///
/// `detail` carries **what to do next**. The caller here is usually an AI rather than a
/// person, and given only "unknown target" it starts inventing names and retrying. Hand it
/// the list of names that do exist and it corrects itself on the spot.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    pub detail: Option<Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> ApiError {
        ApiError { status, message: message.into(), detail: None }
    }
    pub fn bad_request(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, message)
    }
    pub fn not_found(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::NOT_FOUND, message)
    }
    pub fn conflict(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::CONFLICT, message)
    }
    pub fn forbidden(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::FORBIDDEN, message)
    }
    pub fn internal(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
    pub fn with_detail(mut self, detail: Value) -> ApiError {
        self.detail = Some(detail);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match self.detail {
            Some(d) => json!({"error": self.message, "detail": d}),
            None => json!({"error": self.message}),
        };
        json_response(self.status, &body)
    }
}

pub fn json_response(status: StatusCode, body: &Value) -> Response {
    let text = serde_json::to_string_pretty(body).unwrap_or_else(|_| "{}".to_string());
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (status, headers, text).into_response()
}

pub fn json_ok(body: Value) -> Response {
    json_response(StatusCode::OK, &body)
}

/// A PNG body. The metadata rides in a header, so a call that writes the bytes straight to a
/// file (`curl -o`) can still tell what was captured.
pub fn png_response(bytes: Vec<u8>, meta: &Value) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // HTTP headers carry ASCII only. A window title with non-ASCII characters in it makes
    // header construction fail, and then the metadata is missing entirely. Downgrading those
    // to `?` chooses **metadata that is always there** over metadata that is exact (when the
    // exact value matters, the JSON endpoint has it).
    let ascii: String = meta
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '?' })
        .collect();
    if let Ok(v) = HeaderValue::from_str(&ascii) {
        headers.insert("x-deescreen-meta", v);
    }
    (StatusCode::OK, headers, bytes).into_response()
}
