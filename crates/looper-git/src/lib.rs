#![allow(clippy::type_complexity)]
//! Git worktree management and safety.
//!
//! This crate wraps git CLI operations (matching the Go implementation's
//! `shell.Run` pattern) for creating, restoring, cleaning, and inspecting
//! git worktrees. All commands are executed via `tokio::process::Command`.
//!
//! # Key types
//!
//! - [`Gateway`]: Main struct wrapping git CLI operations.
//! - [`GatewayOptions`]: Configuration for Gateway construction.
//! - [`CheckoutMode`]: Whether a worktree is on a branch or detached HEAD.
//! - [`WorktreeListEntry`]: Parsed entry from `git worktree list --porcelain`.
//!
//! # Safety
//!
//! - [`assert_writable_branch`]: Guard against mutating protected branches.
//! - [`validate_worktree_path`]: Symlink-aware path safety validation.

pub mod error;
pub mod gateway;
pub mod helpers;
pub mod safety;
pub mod types;

pub use error::{GitError, ProtectedBranchError, RemoteHeadChangedError, Result};
pub use gateway::Gateway;
pub use safety::{assert_writable_branch, validate_worktree_path, SafetyCheckInput};
pub use types::{
    build_worktree_directory_name, sanitize_branch_name, CheckoutMode, CleanupWorktreeInput, CommitInput, CommitResult,
    CreateWorktreeInput, CreateWorktreeResult, GatewayOptions, InspectHeadInput, InspectHeadResult,
    PrepareWorktreeInput, PrepareWorktreeResult, PushInput, RestoreWorktreeInput, WorktreeListEntry,
};
