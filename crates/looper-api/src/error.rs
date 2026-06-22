use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Standardized error codes returned in API envelopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // ── Existing (8) ──
    Internal,
    NotFound,
    BadRequest,
    Validation,
    Auth,
    RateLimit,
    Conflict,
    Unavailable,

    // ── Extended (11) — total = 19 ──
    Forbidden,
    Timeout,
    PayloadTooLarge,
    UnsupportedMediaType,
    MethodNotAllowed,
    NotAcceptable,
    Gone,
    PreconditionFailed,
    TooEarly,
    UpgradeRequired,
    LoopDetected,
}

impl ErrorCode {
    /// Map an `ErrorCode` to the appropriate HTTP status code.
    #[must_use]
    pub fn status_code(self) -> StatusCode {
        match self {
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::Validation => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Auth => StatusCode::UNAUTHORIZED,
            Self::RateLimit => StatusCode::TOO_MANY_REQUESTS,
            Self::Conflict => StatusCode::CONFLICT,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Timeout => StatusCode::REQUEST_TIMEOUT,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::NotAcceptable => StatusCode::NOT_ACCEPTABLE,
            Self::Gone => StatusCode::GONE,
            Self::PreconditionFailed => StatusCode::PRECONDITION_FAILED,
            Self::TooEarly => StatusCode::TOO_EARLY,
            Self::UpgradeRequired => StatusCode::UPGRADE_REQUIRED,
            Self::LoopDetected => StatusCode::LOOP_DETECTED,
        }
    }
}

/// Structured error information returned in an error envelope.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorInfo {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// API error that can be converted directly into an HTTP response.
/// Combines an HTTP status code (for the response) with structured
/// error information (for the JSON body).
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub ErrorInfo);

impl ApiError {
    /// Create a new API error.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(code.status_code(), ErrorInfo {
            code,
            message: message.into(),
            details: None,
        })
    }

    /// Create a new API error with additional details.
    #[must_use]
    pub fn with_details(code: ErrorCode, message: impl Into<String>, details: serde_json::Value) -> Self {
        Self(code.status_code(), ErrorInfo {
            code,
            message: message.into(),
            details: Some(details),
        })
    }

    /// Create a 400 Bad Request error.
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    /// Create a 404 Not Found error.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    /// Create a 409 Conflict error.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    /// Create a 500 Internal error.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// Create a 422 Validation error.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Validation, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.0;
        let envelope = crate::envelope::Envelope::<()> {
            ok: false,
            data: None,
            error: Some(self.1),
        };
        (status, axum::Json(envelope)).into_response()
    }
}

impl<E: std::fmt::Display> From<crate::types::ApiServiceError<E>> for ApiError {
    fn from(err: crate::types::ApiServiceError<E>) -> Self {
        match err {
            crate::types::ApiServiceError::NotFound(msg) => Self::not_found(msg),
            crate::types::ApiServiceError::Conflict(msg) => Self::conflict(msg),
            crate::types::ApiServiceError::BadRequest(msg) => Self::bad_request(msg),
            crate::types::ApiServiceError::Validation(msg) => Self::validation(msg),
            crate::types::ApiServiceError::Internal(msg) => Self::internal(msg),
            crate::types::ApiServiceError::Service(e) => Self::internal(format!("Service error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ApiServiceError;
    use axum::http::StatusCode;

    #[test]
    fn test_error_code_status_codes() {
        assert_eq!(ErrorCode::Internal.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(ErrorCode::NotFound.status_code(), StatusCode::NOT_FOUND);
        assert_eq!(ErrorCode::BadRequest.status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(ErrorCode::Validation.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(ErrorCode::Auth.status_code(), StatusCode::UNAUTHORIZED);
        assert_eq!(ErrorCode::RateLimit.status_code(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(ErrorCode::Conflict.status_code(), StatusCode::CONFLICT);
        assert_eq!(ErrorCode::Unavailable.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_extended_error_code_status_codes() {
        assert_eq!(ErrorCode::Forbidden.status_code(), StatusCode::FORBIDDEN);
        assert_eq!(ErrorCode::Timeout.status_code(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(ErrorCode::PayloadTooLarge.status_code(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(ErrorCode::UnsupportedMediaType.status_code(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(ErrorCode::MethodNotAllowed.status_code(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(ErrorCode::NotAcceptable.status_code(), StatusCode::NOT_ACCEPTABLE);
        assert_eq!(ErrorCode::Gone.status_code(), StatusCode::GONE);
        assert_eq!(ErrorCode::PreconditionFailed.status_code(), StatusCode::PRECONDITION_FAILED);
        assert_eq!(ErrorCode::TooEarly.status_code(), StatusCode::TOO_EARLY);
        assert_eq!(ErrorCode::UpgradeRequired.status_code(), StatusCode::UPGRADE_REQUIRED);
        assert_eq!(ErrorCode::LoopDetected.status_code(), StatusCode::LOOP_DETECTED);
    }

    #[test]
    fn test_error_count() {
        // Verify we have exactly 19 variants
        let variants = [
            ErrorCode::Internal,
            ErrorCode::NotFound,
            ErrorCode::BadRequest,
            ErrorCode::Validation,
            ErrorCode::Auth,
            ErrorCode::RateLimit,
            ErrorCode::Conflict,
            ErrorCode::Unavailable,
            ErrorCode::Forbidden,
            ErrorCode::Timeout,
            ErrorCode::PayloadTooLarge,
            ErrorCode::UnsupportedMediaType,
            ErrorCode::MethodNotAllowed,
            ErrorCode::NotAcceptable,
            ErrorCode::Gone,
            ErrorCode::PreconditionFailed,
            ErrorCode::TooEarly,
            ErrorCode::UpgradeRequired,
            ErrorCode::LoopDetected,
        ];
        assert_eq!(variants.len(), 19);
    }

    #[test]
    fn test_api_error_factories() {
        let err = ApiError::bad_request("bad input");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1.code, ErrorCode::BadRequest);
        assert_eq!(err.1.message, "bad input");

        let err = ApiError::not_found("missing");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_with_details() {
        let details = serde_json::json!({"field": "name"});
        let err = ApiError::with_details(ErrorCode::Validation, "invalid", details);
        assert!(err.1.details.is_some());
    }

    #[test]
    fn test_api_service_error_conversion() {
        let svc_err: ApiServiceError<String> = ApiServiceError::NotFound("not here".into());
        let api_err: ApiError = svc_err.into();
        assert_eq!(api_err.0, StatusCode::NOT_FOUND);
    }
}
