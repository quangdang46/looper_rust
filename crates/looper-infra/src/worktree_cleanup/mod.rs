//! Worktree cleanup subsystem — plans and executes cleanup of stale git worktrees.

mod plan;
mod run;

pub use plan::{plan, Decision, DecisionAction, PlanResult, Summary};
pub use run::{run, CleanupOptions, RunResult};

use looper_storage::repos::Repositories;
use std::sync::Arc;

use crate::error::CleanupError;

// --------------------------------------------------------------------------
// Public API
// --------------------------------------------------------------------------

/// Run a full plan → execute cycle.
pub fn run_cycle(
    repos: &Arc<Repositories>,
    options: &CleanupOptions,
) -> Result<RunResult, CleanupError> {
    let plan_result = plan(repos, options)?;
    let run_result = run(repos, &plan_result, options)?;
    Ok(run_result)
}
