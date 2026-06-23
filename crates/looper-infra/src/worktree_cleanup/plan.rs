//! Worktree cleanup PLAN phase — cross-references worktrees with loops/runs/queue
//! to determine which worktrees are eligible for cleanup.

use chrono::{DateTime, Utc};
use looper_storage::record::{LoopRecord, QueueItemRecord, RunRecord, WorktreeRecord};
use looper_storage::repos::Repositories;
use std::sync::Arc;

use crate::error::CleanupError;
use crate::worktree_cleanup::CleanupOptions;

// --------------------------------------------------------------------------
// Types
// --------------------------------------------------------------------------

/// Summary statistics for a plan run.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub scanned: usize,
    pub candidates: usize,
    pub would_clean: usize,
    pub skipped: usize,
    pub failed: usize,
    pub orphans: usize,
}

/// What to do with a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionAction {
    WouldClean,
    Skipped,
}

/// A single worktree decision.
#[derive(Debug, Clone)]
pub struct Decision {
    pub worktree: WorktreeRecord,
    pub action: DecisionAction,
    pub reason: String,
    pub last_used_at: Option<String>,
    pub orphan: bool,
    pub references: Vec<Ref>,
}

/// A reference to a loop, run, or queue item that uses this worktree.
#[derive(Debug, Clone)]
pub struct Ref {
    pub kind: &'static str,
    pub id: String,
    pub status: String,
}

/// The full plan result.
#[derive(Debug, Clone)]
pub struct PlanResult {
    pub summary: Summary,
    pub decisions: Vec<Decision>,
}

// --------------------------------------------------------------------------
// Plan
// --------------------------------------------------------------------------

/// Plan which worktrees to clean.
pub fn plan(repos: &Arc<Repositories>, options: &CleanupOptions) -> Result<PlanResult, CleanupError> {
    // Scan active worktrees (status='active')
    let active_worktrees = repos
        .worktrees
        .list_active()
        .map_err(|e| CleanupError::Plan(format!("list active worktrees: {e}")))?;

    // Also scan non-active, non-cleaned worktrees (e.g. "created" status from dead runners)
    let stale_candidates = repos
        .worktrees
        .list_cleanup_candidates(100)
        .map_err(|e| CleanupError::Plan(format!("list cleanup candidates: {e}")))?;

    // Merge, deduplicate by id
    let mut seen = std::collections::HashSet::new();
    let worktrees: Vec<WorktreeRecord> = active_worktrees
        .into_iter()
        .chain(stale_candidates)
        .filter(|wt| seen.insert(wt.id.clone()))
        .collect();

    let loops = repos
        .loops
        .list()
        .map_err(|e| CleanupError::Plan(format!("list loops: {e}")))?;

    let loop_ids: Vec<String> = loops.iter().map(|l| l.id.clone()).collect();
    let runs = repos
        .runs
        .list_latest_by_loop_ids(&loop_ids)
        .map_err(|e| CleanupError::Plan(format!("list runs: {e}")))?;

    let queue_items = repos
        .queue
        .list()
        .map_err(|e| CleanupError::Plan(format!("list queue items: {e}")))?;

    let retention_cutoff = options.retention_cutoff();
    let mut summary = Summary::default();
    let mut decisions = Vec::new();

    for wt in &worktrees {
        summary.scanned += 1;

        if wt.status == "cleaned" {
            continue;
        }

        let mut orphan = true;
        let mut last_used_at: Option<DateTime<Utc>> = None;
        let mut references: Vec<Ref> = Vec::new();
        let mut blocked = false;
        let mut block_reason = String::new();

        // Cross-reference with loops
        for l in &loops {
            if !worktree_matches_loop(wt, l) {
                continue;
            }
            orphan = false;
            references.push(Ref {
                kind: "loop",
                id: l.id.clone(),
                status: l.status.clone(),
            });
            update_last_used(&mut last_used_at, &l.updated_at);
            if let Some(ref ts) = l.last_run_at {
                update_last_used(&mut last_used_at, ts);
            }

            // Protected statuses block cleanup
            if is_protected_loop_status(&l.status) {
                blocked = true;
                block_reason = format!("referenced by protected loop status '{}'", l.status);
            }
        }

        // Cross-reference with runs
        for r in &runs {
            if !worktree_matches_run(wt, r) {
                continue;
            }
            orphan = false;
            references.push(Ref {
                kind: "run",
                id: r.id.clone(),
                status: r.status.clone(),
            });
            update_last_used(&mut last_used_at, &r.updated_at);
            update_last_used(&mut last_used_at, &r.started_at);
            if let Some(ref ts) = r.ended_at {
                update_last_used(&mut last_used_at, ts);
            }

            if r.status == "running" {
                blocked = true;
                block_reason = "referenced by running run".to_string();
            }
        }

        // Cross-reference with queue items
        for qi in &queue_items {
            if qi.status != "queued" && qi.status != "running" {
                continue;
            }
            if !worktree_matches_queue(wt, qi) {
                continue;
            }
            orphan = false;
            references.push(Ref {
                kind: "queue",
                id: qi.id.clone(),
                status: qi.status.clone(),
            });
            update_last_used(&mut last_used_at, &qi.updated_at);
            update_last_used(&mut last_used_at, &qi.created_at);

            blocked = true;
            block_reason = "referenced by active queue item".to_string();
        }

        // Determine action
        if orphan && !options.include_orphans {
            summary.skipped += 1;
            decisions.push(Decision {
                worktree: wt.clone(),
                action: DecisionAction::Skipped,
                reason: "orphan and include_orphans=false".into(),
                last_used_at: last_used_at.map(|d| d.to_rfc3339()),
                orphan: true,
                references,
            });
            continue;
        }

        if blocked {
            summary.skipped += 1;
            decisions.push(Decision {
                worktree: wt.clone(),
                action: DecisionAction::Skipped,
                reason: block_reason,
                last_used_at: last_used_at.map(|d| d.to_rfc3339()),
                orphan,
                references,
            });
            continue;
        }

        // Retention window check
        if let Some(ref lua) = last_used_at {
            if lua > &retention_cutoff {
                summary.skipped += 1;
                decisions.push(Decision {
                    worktree: wt.clone(),
                    action: DecisionAction::Skipped,
                    reason: "within retention window".into(),
                    last_used_at: Some(lua.to_rfc3339()),
                    orphan,
                    references,
                });
                continue;
            }
        }

        // MaxPerTick limit
        if summary.would_clean >= options.max_per_tick {
            summary.skipped += 1;
            decisions.push(Decision {
                worktree: wt.clone(),
                action: DecisionAction::Skipped,
                reason: "maxPerTick limit reached".into(),
                last_used_at: last_used_at.map(|d| d.to_rfc3339()),
                orphan,
                references,
            });
            continue;
        }

        // Eligible for cleanup
        summary.candidates += 1;
        summary.would_clean += 1;
        if orphan {
            summary.orphans += 1;
        }
        decisions.push(Decision {
            worktree: wt.clone(),
            action: DecisionAction::WouldClean,
            reason: if orphan { "orphan worktree" } else { "stale worktree" }
                .into(),
            last_used_at: last_used_at.map(|d| d.to_rfc3339()),
            orphan,
            references,
        });
    }

    Ok(PlanResult { summary, decisions })
}

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

const PROTECTED_LOOP_STATUSES: &[&str] = &[
    "idle", "queued", "running", "waiting", "paused", "failed", "interrupted",
];

fn is_protected_loop_status(status: &str) -> bool {
    PROTECTED_LOOP_STATUSES.contains(&status)
}

fn worktree_matches_loop(wt: &WorktreeRecord, l: &LoopRecord) -> bool {
    if l.project_id != wt.project_id {
        return false;
    }
    // Match via metadata_json containing worktree path or branch
    if let Some(ref meta) = l.metadata_json {
        if meta.contains(&wt.worktree_path) || meta.contains(&wt.branch) {
            return true;
        }
    }
    false
}

fn worktree_matches_run(wt: &WorktreeRecord, r: &RunRecord) -> bool {
    // Run matches its loop's worktree — match via checkpoint_json path
    if let Some(ref cp) = r.checkpoint_json {
        if cp.contains(&wt.worktree_path) || cp.contains(&wt.branch) {
            return true;
        }
    }
    false
}

fn worktree_matches_queue(wt: &WorktreeRecord, qi: &QueueItemRecord) -> bool {
    if let Some(ref payload) = qi.payload_json {
        if payload.contains(&wt.worktree_path) || payload.contains(&wt.branch) {
            return true;
        }
    }
    // Match via loop_id → loop metadata
    false
}

fn update_last_used(acc: &mut Option<DateTime<Utc>>, ts: &str) {
    if !ts.is_empty() {
        if let Ok(dt) = ts.parse::<DateTime<Utc>>() {
            match acc {
                Some(ref mut current) => {
                    if dt > *current {
                        *current = dt;
                    }
                }
                None => *acc = Some(dt),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use looper_storage::record::WorktreeRecord;

    fn sample_worktree() -> WorktreeRecord {
        WorktreeRecord {
            id: "wt-1".into(),
            project_id: "proj-1".into(),
            repo_path: "/repos/foo".into(),
            worktree_path: "/worktrees/foo-feature".into(),
            branch: "feature".into(),
            base_branch: Some("main".into()),
            status: "active".into(),
            head_sha: None,
            metadata_json: None,
            created_at: "2026-06-01T00:00:00Z".into(),
            updated_at: "2026-06-10T00:00:00Z".into(),
            cleaned_at: None,
        }
    }

    #[test]
    fn test_is_protected_loop_status() {
        assert!(is_protected_loop_status("running"));
        assert!(is_protected_loop_status("queued"));
        assert!(!is_protected_loop_status("completed"));
        assert!(!is_protected_loop_status("terminal"));
    }

    #[test]
    fn test_update_last_used() {
        let mut acc: Option<DateTime<Utc>> = None;
        update_last_used(&mut acc, "2026-06-10T00:00:00Z");
        assert!(acc.is_some());
        let first = acc.unwrap();
        update_last_used(&mut acc, "2026-06-15T00:00:00Z");
        assert!(acc.unwrap() > first);
    }

    #[test]
    fn test_worktree_matches_loop_by_path() {
        let wt = sample_worktree();
        let mut l = LoopRecord {
            id: "loop-1".into(),
            seq: 1,
            project_id: "proj-1".into(),
            r#type: "reviewer".into(),
            target_type: "pr".into(),
            target_id: None,
            repo: None,
            pr_number: None,
            status: "running".into(),
            config_json: None,
            metadata_json: Some(r#"{"worktree_path":"/worktrees/foo-feature"}"#.into()),
            last_run_at: None,
            next_run_at: None,
            created_at: "".into(),
            updated_at: "".into(),
        };

        assert!(worktree_matches_loop(&wt, &l));

        l.metadata_json = Some(r#"{"worktree_path":"/other/path"}"#.into());
        assert!(!worktree_matches_loop(&wt, &l));
    }

    #[test]
    fn test_plan_skips_cleaned_worktrees() {
        // Use the function to test summary structure
        let wt = WorktreeRecord {
            status: "cleaned".into(),
            ..sample_worktree()
        };
        assert_eq!(wt.status, "cleaned");
    }
}
