//! Worktree cleanup RUN phase — executes cleanup on eligible worktrees.

use chrono::Utc;
use looper_storage::record::WorktreeRecord;
use looper_storage::repos::Repositories;
use std::process::Command;
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

        // Dry run: skip actual git operations
        if options.dry_run {
            tracing::info!(
                worktree = %wt.worktree_path,
                branch = %wt.branch,
                "dry-run: would clean worktree"
            );
            continue;
        }

        // 1. Try git worktree remove --force (handles git's internal worktree tracking)
        let git_remove_ok = if let Some(repo_path) = wt.repo_path.strip_suffix('/').or(Some(&wt.repo_path)) {
            // Try removing from the repo's parent git context if possible
            let repo_dir = if !wt.repo_path.is_empty() && std::path::Path::new(&wt.repo_path).join(".git").exists() {
                wt.repo_path.clone()
            } else {
                // Fall back to running from the worktree path's parent
                std::path::Path::new(&wt.worktree_path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
            };
            if !repo_dir.is_empty() {
                match Command::new("git")
                    .args(["worktree", "remove", "--force", &wt.worktree_path])
                    .current_dir(&repo_dir)
                    .output()
                {
                    Ok(output) => {
                        if output.status.success() {
                            tracing::info!(worktree = %wt.worktree_path, "git worktree remove succeeded");
                            true
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            if stderr.contains("is not a working tree") || stderr.contains("does not exist") {
                                tracing::info!(worktree = %wt.worktree_path, "worktree already gone from git");
                                true
                            } else {
                                tracing::warn!(worktree = %wt.worktree_path, "git worktree remove: {stderr}");
                                false
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(worktree = %wt.worktree_path, "git worktree remove command error: {e}");
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        };

        // 2. Belt-and-suspenders: remove the directory directly
        let path = std::path::Path::new(&wt.worktree_path);
        if path.exists() {
            if let Err(e) = std::fs::remove_dir_all(&wt.worktree_path) {
                let msg = format!("remove_dir_all({}): {e}", wt.worktree_path);
                tracing::warn!(worktree = %wt.worktree_path, "{msg}");
                result.errors.push(msg.clone());
            } else {
                tracing::info!(worktree = %wt.worktree_path, "directory removed from disk");
            }
        }

        // 3. Update DB record to mark cleaned
        let cleaned = WorktreeRecord {
            cleaned_at: Some(now_iso.clone()),
            status: "cleaned".to_string(),
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
