//! E2E test harness for the Looper daemon.
//!
//! Ported from the Go original at `legacy/internal/e2e/harness/`.
//! Provides temporary test homes, port allocation, binaries discovery,
//! configuration helpers, and fake GitHub/Agent/osascript subprocess
//! types used by integration-style tests.

// Keep unused-crate-deps linter quiet — some deps are consumed only at the
// binary level but declared in this crate's manifest, or may be used by
// test helpers in future iterations.
#[allow(unused_imports)]
use {
    anyhow as _, chrono as _, looper_agent as _, looper_git as _, looper_github as _, looper_runner as _,
    looper_scheduler as _, looper_service as _, looper_storage as _, thiserror as _, tokio as _,
};

pub mod artifacts;
pub mod assertions;
pub mod binaries;
pub mod config;
pub mod daemon;
pub mod fake_agent;
pub mod fake_gh;
pub mod git;
pub mod ports;
pub mod temp_home;

pub use artifacts::{artifact_base_dir, artifact_temp_dir};
pub use assertions::{assert_cwd_inside_worktree, assert_repo_unchanged, load_cwd_evidence, CWDEvidence};
pub use binaries::{must_binaries, BuiltBinaries};
pub use config::{default_config, write_config, ConfigOptions, TestToolPaths};
pub use fake_agent::FakeAgent;
pub use fake_gh::{FakeGH, GHPullRequest, GHSchema, GHState, GHThread, GHThreadComment};
pub use git::{create_seeded_repo, run_git, snapshot_repo, RepoSnapshot, SeededRepo};
pub use ports::{base_url, must_free_port};
pub use temp_home::TempHome;
