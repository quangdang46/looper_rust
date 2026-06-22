#![allow(clippy::type_complexity)]
//! Scheduler: tick loop, queue claim, discovery orchestration, and recovery.
//!
//! This crate implements the core scheduler loop that:
//! - Runs a periodic tick that iterates projects and discovers work
//! - Maintains an independent claim pump for fast queue consumption
//! - Dispatches claimed queue items to role-specific runners
//! - Handles startup recovery (orphan cleanup, stale runs, lock release)
//! - Classifies failures for retry decisions

pub mod active_executions;
pub mod claim;
pub mod error;
pub mod failure;
pub mod recovery;
pub mod scheduler;
pub mod tick;
pub mod types;

pub use active_executions::ActiveExecutionRegistry;
pub use claim::claim_and_run;
pub use error::{SchedulerError, SchedulerResult};
pub use failure::{classify_failure, classify_by_boundary, compute_retry_delay, should_retry_queue_item, step_boundary};
pub use recovery::{reconcile_stale_runs, run_recovery, RecoverySummary};
pub use scheduler::Scheduler;
pub use tick::{execute_claim_phase, execute_scheduler_tick};
pub use types::*;
