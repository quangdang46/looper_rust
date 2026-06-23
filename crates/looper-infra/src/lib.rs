#![allow(clippy::type_complexity)]
//! # looper-infra
//!
//! Daemon bootstrap, runtime lifecycle, notifications, and worktree cleanup.
//!
//! ## Modules
//!
//! - [`bootstrap`] — Tool validation, directory setup, logger creation.
//! - [`runtime`] — [`Runtime`] struct with start/stop/shutdown lifecycle.
//! - [`notifications`] — Notification gateway, database/osascript backends, throttle.
//! - [`worktree_cleanup`] — Plan/execute cleanup of stale git worktrees.

#[allow(unused_imports)]
use {looper_runner as _, looper_service as _, looper_types as _, serde as _, uuid as _};

pub mod bootstrap;
pub mod circuit_breaker;
pub mod daemon_lock;
pub mod error;
pub mod forwarder_supervisor;
pub mod notifications;
pub mod recovery;
pub mod runtime;
pub mod worktree_cleanup;
pub mod shell;

pub use bootstrap::BootstrapOutput;
pub use error::{BootError, CleanupError, DirError, NotifyError, RuntimeError, SetupError};
pub use runtime::{Runtime, Services};
pub use worktree_cleanup::run_cycle;
pub use shell::CommandResult;

pub use circuit_breaker::CircuitBreaker;
pub use daemon_lock::DaemonLock;
pub use notifications::{CompositeGateway, Gateway, Notification, NotificationLevel};
