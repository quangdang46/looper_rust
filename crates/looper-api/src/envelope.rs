use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::ErrorInfo;

/// Standard API response envelope.
///
/// Every endpoint returns a JSON body shaped like this:
/// ```json
/// { "ok": true, "data": { ... }, "error": null }
/// ```
///
/// On failure the `error` field contains structured error information
/// and `data` is absent.
#[derive(Debug, Serialize)]
pub struct Envelope<T: Serialize> {
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<ErrorInfo>,
}

impl<T: Serialize> Envelope<T> {
    /// Build a success envelope.
    #[must_use]
    pub fn success(data: T) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    /// Build a success envelope whose data is an empty JSON object `{}`.
    /// Useful for endpoints that have no meaningful response body.
    #[must_use]
    pub fn success_empty() -> Self
    where
        T: Default,
    {
        Self {
            ok: true,
            data: Some(T::default()),
            error: None,
        }
    }
}

impl<T: Serialize> IntoResponse for Envelope<T> {
    fn into_response(self) -> Response {
        // Derive status code from the error field if present.
        let status = match &self.error {
            Some(info) => info.code.status_code(),
            None => StatusCode::OK,
        };
        (status, axum::Json(self)).into_response()
    }
}
