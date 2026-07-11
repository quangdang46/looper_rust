#![allow(clippy::type_complexity)]
//! axum REST API server with auth, SSE, and structured envelope responses.
//!
//! # Crate structure
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`error`] | `ApiError`, `ErrorCode`, `ErrorInfo` |
//! | [`envelope`] | `Envelope<T>` response wrapper |
//! | [`types`] | `Context`, traits, request/response types |
//! | [`auth`] | Loopback + Bearer-token middleware |
//! | [`sse`] | Server-sent events stream helpers |
//! | [`routes`] | All 30+ endpoint handlers |
//! | [`server`] | Router construction and graceful-shutdown |
//! | [`helpers`] | Small utility functions |

#[allow(unused_imports)]
use {looper_scheduler as _, looper_service as _, looper_types as _, thiserror as _};

pub mod auth;
pub mod envelope;
pub mod error;
pub mod helpers;
pub mod sse;
pub mod types;

pub mod routes;
pub mod server;

pub use envelope::Envelope;
pub use error::{ApiError, ErrorCode, ErrorInfo};
pub use server::{build_router, serve, ServerConfig};
pub use types::{
    AcquireLockInput, AddProjectInput, AgentConfigResponse, ConfigResponse, Context, CreateLoopInput, EnqueueInput,
    EventLogResponse, HealthResponse, LockResponse, LoopDetail, LoopSummary, PaginationParams, ProjectService,
    ProjectSummary, QueueItemResponse, RunDetail, RunSummary, RuntimeState, StartRunInput, UpdateProjectInput,
    VersionResponse,
};
