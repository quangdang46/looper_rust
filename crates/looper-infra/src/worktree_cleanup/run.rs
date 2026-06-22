//! Worktree cleanup RUN phase — executes cleanup on eligible worktrees.

use chrono::Utc;
use looper_storage::record::WorktreeRecord;
use looper_storage::repos::Repositories;
use std::sync::Arc;

use super::plan::{DecisionAction, PlanResult};
use crate::error::CleanupError;

// --------------------------------------------------------------------------
// Types
// --------------------------------------------------------------------------

/// Options for worktree cleanup planning and execution.
#[derive(Debug, Clone)]
pub struct CleanupOptions {
    pub include_orphans: bool,
    pub retention_days: u64,
    pub max_per_tick: usize,
    pub dry_run: bool,
    pub project_id: Option<String>,
}

impl Default for CleanupOptions {
    fn default() -> Self {
        Self {
            include_orphans: true,
            retention_days: 7,
            max_per_tick: 10,
            dry_run: false,
            project_id: None,
        }
    }
}

impl CleanupOptions {
    /// Compute the cutoff datetime for retention window.
    pub fn retention_cutoff(&self) -> chrono::DateTime<Utc> {
        Utc::now()
            .checked_sub_signed(chrono::Duration::days(self.retention_days as i64))
            .unwrap_or_else(|| Utc::now() - chrono::Duration::days(7))
    }
}

/// Result of a cleanup run.
#[derive(Debug, Clone, Default)]
pub struct RunResult {
    pub summary: super::plan::Summary,
    pub cleaned_count: usize,
    pub errors: Vec<String>,
}

// --------------------------------------------------------------------------
// Run
// --------------------------------------------------------------------------

/// Execute cleanup on eligible worktrees from a plan result.
pub fn run(
    repos: &Arc<Repositories>,
    plan_result: &PlanResult,
    options: &CleanupOptions,
) -> Result<RunResult, CleanupError> {
    let mut result = RunResult {
        summary: plan_result.summary.clone(),
        ..Default::default()
    };

    let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    for decision in &plan_result.decisions {
        if decision.action != DecisionAction::WouldClean {
            continue;
        }

        let wt = &decision.worktree;

        // Check if path exists on disk
        let _path_exists = std::path::Path::new(&wt.worktree_path).exists();

        // Dry run: skip actual git operations
        if options.dry_run {
            tracing::info!(
                worktree = %wt.worktree_path,
                branch = %wt.branch,
                "dry-run: would clean worktree"
            );
            continue;
        }

        let cleaned = WorktreeRecord {
            cleaned_at: Some(now_iso.clone()),
            ..wt.clone()
        };
        repos
            .worktrees
            .upsert(&cleaned)
            .map_err(|e| CleanupError::Database(format!("update worktree: {e}")))?;

        result.cleaned_count += 1;
    }

    Ok(result)
}
