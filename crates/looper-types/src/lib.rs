#![allow(clippy::type_complexity)]
//! Core domain types, enums, and state machines for the Looper daemon.
//!
//! This crate has zero IO dependencies — pure type definitions only.
//! Every other crate depends on `looper-types` for shared domain concepts.

pub mod error;
pub mod failure;
pub mod loop_status;
pub mod loop_target;
pub mod loop_type;
pub mod resume;
pub mod retry_policy;
pub mod run_status;
pub mod steps;
pub mod vendor;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use error::DomainError;
pub use failure::FailureKind;
pub use loop_status::LoopStatus;
pub use loop_target::{loop_target_key, LoopTarget, LoopTargetType};
pub use loop_type::LoopType;
pub use resume::ResumePolicy;
pub use retry_policy::RetryPolicy;
pub use run_status::RunStatus;
pub use steps::assert_step_belongs_to_loop_type;
pub use vendor::{AgentVendor, AuthMode, DaemonMode, LogLevel};
