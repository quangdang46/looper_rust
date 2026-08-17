//! Embedded web dashboard — serves the React SPA from compiled assets.

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "dashboard-assets"]
struct DashboardAssets;

/// Serve the dashboard SPA at `/dashboard/`.
pub fn router() -> Router {
    Router::new()
        .route("/dashboard", axum::routing::get(dashboard_root))
        .route("/dashboard/{*path}", axum::routing::get(serve_asset))
}

async fn dashboard_root() -> Response {
    redirect_to("/dashboard/").await
}

async fn redirect_to(path: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, path)]).into_response()
}

async fn serve_asset(Path(path): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    // Strip leading slash — rust-embed paths are relative
    let rel = path.strip_prefix('/').unwrap_or(&path);

    // For SPA: serve index.html for non-asset routes
    let file_path = if rel.is_empty() || rel == "dashboard" { "index.html" } else { rel };

    match DashboardAssets::get(file_path) {
        Some(content) => {
            let mime = mime_guess::from_path(file_path).first_or_octet_stream();
            Ok(([(header::CONTENT_TYPE, mime.to_string())], content.data.to_vec()))
        }
        None => {
            // SPA fallback: serve index.html for client-side routing
            match DashboardAssets::get("index.html") {
                Some(content) => {
                    let mime = "text/html; charset=utf-8";
                    Ok(([(header::CONTENT_TYPE, mime.to_string())], content.data.to_vec()))
                }
                None => Err(StatusCode::NOT_FOUND),
            }
        }
    }
}
