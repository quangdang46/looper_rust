#![allow(clippy::type_complexity)]
//! Loop, Run, and Project lifecycle business logic.
//!
//! This crate provides the service layer that sits between the API handlers
//! and the storage layer.  It encodes domain business rules, state-machine
//! transitions, event log integration, and project lifecycle management.
//!
//! # Services
//!
//! - [`LoopService`](loop_service::LoopService) — Create, transition, pause,
//!   terminate, resume loops.  Policy helpers for failure recovery decisions.
//! - [`RunService`](run_service::RunService) — Start, step-record, complete
//!   runs.  Enforces the one-running-run-per-loop invariant.  Emits event log
//!   entries.
//! - [`ProjectService`](project_service::ProjectService) — Add, remove, list,
//!   sync projects.  Handles ID normalization, reviewer auto-merge validation,
//!   and discovery of worktrees / pull requests.
//! - [`AdmitWorkService`](admit_work::AdmitWorkService) — Admit role work:
//!   create/reuse loop + claimable queue item (no HTTP/tick).

#[allow(unused_imports)]
use {serde as _, uuid as _};

pub mod admit_work;
pub mod error;
pub mod loop_service;
pub mod project_service;
pub mod run_service;

pub use admit_work::{AdmitWorkInput, AdmitWorkResult, AdmitWorkService};
pub use error::{Result, ServiceError};
pub use loop_service::{
    CreateInput, LoopService, PauseInput, PauseResult, TerminateInput, TerminateResult, TransitionInput,
};
pub use project_service::{
    effective_project_repo, normalize_repo_spec, repo_from_metadata, resolve_project_repo, AddInput, AddResult,
    BranchProtection, CallbackResult, ProjectService, ProjectServiceCallbacks, PullRequestEntry, RepositorySettings,
    SnapshotMode, UpdateInput, WorktreeEntry,
};
pub use run_service::{CompleteInput, RecordStepInput, RunService, StartInput};
