//! Runtime recovery pipeline — daemon startup recovery.
//!
//! Ported from Go `legacy/internal/runtime/` recovery sections.
//!
//! 3-phase startup recovery:
//! 1. Orphan agent cleanup — scan for agent processes whose parent died
//! 2. Expired lock release — find stale daemon locks, release them
//! 3. Stale run reconciliation — runs with heartbeat > 30min, mark interrupted

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use looper_storage::repos::Repositories;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const STALE_HEARTBEAT_THRESHOLD: Duration = Duration::from_secs(30 * 60);

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Summary of recovery operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct RecoverySummary {
    pub started_at: String,
    pub completed_at: String,
    pub orphan_agent_cleanup: OrphanAgentCleanup,
    pub expired_locks_released: i64,
    pub interrupted_runs_marked: i64,
    pub events_written: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct OrphanAgentCleanup {
    pub attempted: bool,
    pub cleaned_count: i64,
    pub warning: String,
}



// ---------------------------------------------------------------------------
// Phase 1: Orphan agent cleanup
// ---------------------------------------------------------------------------

/// Attempt to clean up orphaned agent processes.
pub fn cleanup_orphan_agents(repos: &Repositories) -> OrphanAgentCleanup {
    let mut summary = OrphanAgentCleanup { attempted: true, cleaned_count: 0, warning: String::new() };

    match repos.agent_executions.list_active() {
        Ok(executions) => {
            for exec in &executions {
                if let Some(pid) = exec.pid {
                    #[cfg(unix)]
                    {
                        let result = std::process::Command::new("kill").arg("-0").arg(pid.to_string()).output();
                        match result {
                            Ok(out) if out.status.success() => continue,
                            _ => {
                                let mut rec = exec.clone();
                                rec.status = "orphaned".into();
                                let _ = repos.agent_executions.upsert(&rec);
                                summary.cleaned_count += 1;
                            }
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        let mut rec = exec.clone();
                        rec.status = "orphaned".into();
                        let _ = repos.agent_executions.upsert(&rec);
                        summary.cleaned_count += 1;
                    }
                }
            }
        }
        Err(e) => {
            summary.warning = format!("list active executions: {e}");
        }
    }

    summary
}

// ---------------------------------------------------------------------------
// Phase 2: Expired lock release
// ---------------------------------------------------------------------------

/// Release all expired daemon locks.
pub fn release_expired_locks(repos: &Repositories, now: DateTime<Utc>) -> i64 {
    let mut released: i64 = 0;
    let now_iso = now.to_rfc3339();
    if let Ok(locks) = repos.locks.list_expired(&now_iso) {
        for lock in locks {
            let _ = repos.locks.release(&lock.key);
            released += 1;
        }
    }
    released
}

// ---------------------------------------------------------------------------
// Phase 3: Stale run reconciliation
// ---------------------------------------------------------------------------

/// Reconcile stale runs: runs with heartbeat older than threshold.
pub fn reconcile_stale_runs(repos: &Repositories, now: DateTime<Utc>) -> (i64, Vec<String>) {
    let mut interrupted = 0i64;
    let mut run_ids = Vec::new();
    let threshold_secs = STALE_HEARTBEAT_THRESHOLD.as_secs() as i64;

    if let Ok(runs) = repos.runs.list() {
        for run in &runs {
            let last_heartbeat = match &run.last_heartbeat_at {
                Some(hb) => match hb.parse::<DateTime<Utc>>() {
                    Ok(dt) => dt,
                    Err(_) => continue,
                },
                None => {
                    if let Ok(created) = run.created_at.parse::<DateTime<Utc>>() {
                        if now.signed_duration_since(created).num_seconds() > threshold_secs {
                            interrupted += 1;
                            let mut rec = run.clone();
                            rec.status = "interrupted".into();
                            let _ = repos.runs.upsert(&rec);
                            run_ids.push(run.id.clone());
                        }
                    }
                    continue;
                }
            };
            if now.signed_duration_since(last_heartbeat).num_seconds() > threshold_secs {
                interrupted += 1;
                let mut rec = run.clone();
                rec.status = "interrupted".into();
                let _ = repos.runs.upsert(&rec);
                run_ids.push(run.id.clone());
            }
        }
    }

    (interrupted, run_ids)
}

// ---------------------------------------------------------------------------
// Main recovery orchestrator
// ---------------------------------------------------------------------------

/// Run the full 3-phase recovery pipeline.
pub fn run_recovery(repos: &Repositories) -> RecoverySummary {
    let started_at = Utc::now().to_rfc3339();
    let now = Utc::now();

    let orphan = cleanup_orphan_agents(repos);
    let expired = release_expired_locks(repos, now);
    let (interrupted, _run_ids) = reconcile_stale_runs(repos, now);

    RecoverySummary {
        started_at,
        completed_at: Utc::now().to_rfc3339(),
        orphan_agent_cleanup: orphan,
        expired_locks_released: expired,
        interrupted_runs_marked: interrupted,
        events_written: 0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recovery_summary_default() {
        let s = RecoverySummary::default();
        assert_eq!(s.expired_locks_released, 0);
    }

    #[test]
    fn test_orphan_agent_cleanup_default() {
        let o = OrphanAgentCleanup::default();
        assert!(!o.attempted);
        assert_eq!(o.cleaned_count, 0);
    }
}
