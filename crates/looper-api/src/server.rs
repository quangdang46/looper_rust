use std::sync::Arc;
use std::time::Instant;

use axum::routing::{delete, get, post};
use axum::Router;
use tokio::sync::oneshot;
use tower_http::cors::CorsLayer;

use crate::error::ApiError;
use crate::routes;
use crate::routes::AppState;
use crate::types::Context;

/// Server configuration.
pub struct ServerConfig {
    pub bind_address: String,
    pub auth_token: Option<String>,
}

/// Build and return the axum [`Router`] configured with all API routes.
pub fn build_router(ctx: Arc<Context>) -> Router {
    let started_at = Instant::now();
    let state = Arc::new(AppState { ctx, started_at });

    let router = Router::new()
        // Health & admin
        .route("/health", get(routes::health))
        .route("/version", get(routes::version))
        .route("/shutdown", post(routes::shutdown))
        .route("/reload", post(routes::reload))
        // Projects
        .route("/api/projects", get(routes::list_projects).post(routes::add_project))
        .route(
            "/api/projects/:name",
            get(routes::get_project)
                .put(routes::update_project)
                .delete(routes::remove_project),
        )
        .route("/api/projects/:name/sync", post(routes::sync_project))
        // Loops
        .route(
            "/api/projects/:name/loops",
            get(routes::list_loops).post(routes::create_loop),
        )
        .route("/api/projects/:name/loops/:seq", get(routes::get_loop))
        .route("/api/projects/:name/loops/:seq/pause", post(routes::pause_loop))
        .route("/api/projects/:name/loops/:seq/resume", post(routes::resume_loop))
        .route("/api/projects/:name/loops/:seq/terminate", post(routes::terminate_loop))
        // Runs
        .route(
            "/api/projects/:name/loops/:seq/runs",
            get(routes::list_runs).post(routes::start_run),
        )
        .route(
            "/api/projects/:name/loops/:seq/runs/:run_id",
            get(routes::get_run),
        )
        .route(
            "/api/projects/:name/loops/:seq/runs/:run_id/cancel",
            post(routes::cancel_run),
        )
        // Queue
        .route("/api/projects/:name/queue", get(routes::list_queue))
        .route("/api/projects/:name/queue/enqueue", post(routes::enqueue))
        .route("/api/projects/:name/queue/:item_id", delete(routes::dequeue))
        // Events
        .route("/api/projects/:name/events", get(routes::list_events))
        .route("/api/projects/:name/events/stream", get(routes::project_events_stream))
        .route("/api/events/stream", get(routes::global_events_stream))
        // Locks
        .route("/api/locks", get(routes::list_locks).post(routes::acquire_lock))
        .route("/api/locks/:lock_id", delete(routes::release_lock))
        // Config
        .route("/api/config", get(routes::get_config))
        .route("/api/projects/:name/agent-config", get(routes::get_agent_config))
        // CORS
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Apply auth middleware if a token is configured (actual auth layer added
    // externally by the daemon during `Server::start` or via tower layer).
    router
}

/// Start the API server and run until a shutdown signal is received.
pub async fn serve(
    ctx: Arc<Context>,
    config: ServerConfig,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<(), ApiError> {
    let router = build_router(ctx);
    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to bind to {}: {}", config.bind_address, e)))?;

    tracing::info!(address = %config.bind_address, "API server starting");

    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            shutdown_rx.await.ok();
            tracing::info!("API server shutting down gracefully");
        })
        .await
        .map_err(|e| ApiError::internal(format!("Server error: {e}")))?;

    Ok(())
}
