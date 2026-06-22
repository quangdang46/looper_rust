#![allow(clippy::type_complexity)]
#![allow(clippy::arc_with_non_send_sync)]
//! SQLite storage layer with migrations, repositories, event log, and queue.
//!
//! This crate provides:
//! - **Migrations**: Refinery-based runner with embedded SQL for schema initialization
//! - **Repositories**: 12 typed repositories (Projects, Loops, Runs, AgentExecutions,
//!   PullRequestSnapshots, Events, Locks, Queue, Notifications, Worktrees,
//!   WebhookForwarders, WebhookTunnelHooks)
//! - **Event log**: Structured audit log with correlation tracking and actor defaults
//! - **Record types**: 1:1 SQL row mappings for all tables

pub mod error;
pub mod eventlog;
pub mod helpers;
pub mod migration;
pub mod record;
pub mod repos;

pub use error::{Result, StorageError};
pub use eventlog::EventLog;
pub use migration::run_migrations;
pub use record::*;
pub use repos::Repositories;

#[cfg(test)]
mod tests;
