use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::{ApiError, ErrorCode};

/// Auth state for the middleware layer.
#[derive(Clone)]
pub struct AuthConfig {
    /// The bearer token that remote requests must present.
    /// When `None`, only loopback requests are accepted.
    pub token: Option<String>,
}

/// Axum middleware that enforces API authentication.
///
/// Rules (in order):
/// 1. Requests from loopback addresses (127.0.0.1, ::1, unix socket) are
///    always allowed (no token required).
/// 2. If `AuthConfig.token` is set, the request must present a matching
///    `Authorization: Bearer <token>` header.
/// 3. If `AuthConfig.token` is `None`, remote requests are rejected.
pub async fn auth_middleware(State(auth): State<AuthConfig>, req: Request, next: Next) -> Response {
    // Check if the request arrived over a loopback connection.
    if is_loopback(&req) {
        return next.run(req).await;
    }

    // For remote requests, enforce bearer token.
    match &auth.token {
        Some(expected) => {
            let header = req.headers().get(AUTHORIZATION).and_then(|v| v.to_str().ok());

            match header {
                Some(h) if h == format!("Bearer {expected}") => next.run(req).await,
                _ => ApiError::new(ErrorCode::Auth, "Missing or invalid authentication token").into_response(),
            }
        }
        None => {
            ApiError::new(ErrorCode::Auth, "Remote requests are not allowed (no auth token configured)").into_response()
        }
    }
}

/// Check whether a request arrived over a loopback connection.
///
/// This inspects:
/// - `axum::extract::ConnectInfo<SocketAddr>` extension (set by the
///   `axum::serve` listener)
/// - Proxy headers `X-Forwarded-For` / `X-Real-IP` for deployments behind
///   a reverse proxy (e.g. nginx on the same machine)
fn is_loopback(req: &Request) -> bool {
    // x-real-ip / x-forwarded-for proxy headers
    if let Some(real_ip) = req.headers().get("x-real-ip").and_then(|v| v.to_str().ok()) {
        if is_loopback_addr(real_ip) {
            return true;
        }
    }
    if let Some(forwarded_for) = req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        // X-Forwarded-For can contain a comma-separated list; check the
        // leftmost (original client) address.
        if let Some(client_ip) = forwarded_for.split(',').next().map(|s| s.trim()) {
            if is_loopback_addr(client_ip) {
                return true;
            }
        }
    }

    // axum ConnectInfo extension (set when the server is bound with
    // `serve(addr, ...).into_service()`).
    if let Some(connect_info) = req.extensions().get::<axum::extract::connect_info::ConnectInfo<std::net::SocketAddr>>()
    {
        if connect_info.0.ip().is_loopback() {
            return true;
        }
    }

    false
}

fn is_loopback_addr(addr: &str) -> bool {
    // Strip port if present
    let host = addr.split(':').next().unwrap_or(addr);
    host == "127.0.0.1" || host == "::1" || host == "localhost"
}
