use std::process::Command;

use chrono::{DateTime, Utc};

use crate::error::SchedulerResult;
use crate::types::{Context, StaleRunReconcileMode, StaleRunReconcileSummary};
use looper_storage::record::{AgentExecutionRecord, RunRecord};
use looper_storage::Repositories;

/// Run the full recovery pipeline (5 phases).
pub fn run_recovery(repos: &Repositories, now: DateTime<Utc>, ctx: &Context) -> RecoverySummary {
    tracing::info!("recovery pipeline started");
    let mut summary = RecoverySummary { started_at: Some(now), ..Default::default() };

    let (cleaned, uncertain) = phase_orphan_cleanup(repos, ctx);
    summary.orphan_cleaned = cleaned;
    summary.orphan_uncertain = uncertain;

    if ctx.is_cancelled() {
        return summary;
    }

    summary.expired_locks_released = phase_expired_lock_release(repos, now) as u64;

    if ctx.is_cancelled() {
        return summary;
    }

    match reconcile_stale_runs(repos, now, StaleRunReconcileMode::Startup, ctx) {
        Ok(s) => {
            summary.interrupted_runs = s.interrupted_runs;
            summary.loops_requeued = s.loops_requeued;
            summary.queue_items_requeued = s.queue_items_requeued;
            summary.queue_items_cancelled = s.queue_items_cancelled;
            summary.cleaned_executions = s.cleaned_executions;
            summary.skipped_uncertain_runs = s.skipped_uncertain_runs;
            summary.run_ids = s.run_ids;
            summary.loop_ids = s.loop_ids;
            summary.execution_ids = s.execution_ids;
        }
        Err(e) => tracing::error!("stale run reconciliation failed: {e}"),
    }

    if ctx.is_cancelled() {
        return summary;
    }

    let now_iso = format_javascript_iso_string(now.to_utc());
    match repos.queue.cleanup_stale_queued(&now_iso, "loop terminated") {
        Ok(n) => summary.loop_normalization_events = n as u64,
        Err(e) => tracing::warn!("stale queued cleanup failed: {e}"),
    }

    tracing::info!("recovery pipeline completed");
    summary
}

fn phase_orphan_cleanup(repos: &Repositories, ctx: &Context) -> (u64, u64) {
    let mut cleaned = 0u64;
    let mut uncertain = 0u64;

    let executions = match repos.agent_executions.list_active() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("failed to list active executions: {e}");
            return (0, 0);
        }
    };

    for exec in &executions {
        if ctx.is_cancelled() {
            break;
        }
        if let Some(pid) = exec.pid {
            if pid <= 0 {
                continue;
            }
            match verify_process_identity(pid as u32, &exec.command_json.clone().unwrap_or_default()) {
                Ok((true, true)) => {
                    let _ = kill_process(pid as u32);
                    cleaned += 1;
                }
                Ok((false, _)) => {
                    cleaned += 1;
                }
                Ok((true, false)) => {
                    uncertain += 1;
                }
                Err(e) => {
                    tracing::warn!("process verification error for pid {pid}: {e}");
                    uncertain += 1;
                }
            }
        }
    }

    (cleaned, uncertain)
}

fn phase_expired_lock_release(repos: &Repositories, now: DateTime<Utc>) -> usize {
    let now_iso = format_javascript_iso_string(now.to_utc());
    match repos.locks.list_expired(&now_iso) {
        Ok(locks) => {
            let count = locks.len();
            for lock in &locks {
                if let Err(e) = repos.locks.release(&lock.key) {
                    tracing::warn!("failed to release expired lock {}: {e}", lock.key);
                }
            }
            count
        }
        Err(e) => {
            tracing::error!("failed to list expired locks: {e}");
            0
        }
    }
}

pub fn reconcile_stale_runs(
    repos: &Repositories,
    now: DateTime<Utc>,
    mode: StaleRunReconcileMode,
    ctx: &Context,
) -> SchedulerResult<StaleRunReconcileSummary> {
    let mut summary = StaleRunReconcileSummary { mode, started_at: Some(now), ..Default::default() };

    let running_runs = repos.runs.list_by_status("running")?;
    let active_executions = repos.agent_executions.list_active()?;

    for run in &running_runs {
        if ctx.is_cancelled() {
            break;
        }

        let decision = evaluate_stale_run_candidate(run, &active_executions, now, &summary.mode)?;
        if !decision.candidate {
            continue;
        }
        if decision.uncertain {
            summary.skipped_uncertain_runs += 1;
            continue;
        }

        let now_iso = format_javascript_iso_string(now.to_utc());
        let _ = repos.runs.upsert(&RunRecord {
            status: "interrupted".into(),
            ended_at: Some(now_iso.clone()),
            ..run.clone()
        });
        summary.interrupted_runs += 1;
        summary.run_ids.push(run.id.clone());

        repair_queue_items_for_run(repos, &run.loop_id, &now_iso)?;
        summary.loops_requeued += 1;
        summary.loop_ids.push(run.loop_id.clone());
    }

    summary.completed_at = Some(now);
    Ok(summary)
}

fn repair_queue_items_for_run(repos: &Repositories, loop_id: &str, now_iso: &str) -> SchedulerResult<()> {
    repos.queue.cancel_by_loop(loop_id, now_iso, Some("recovery interrupted"))?;
    repos.queue.requeue_latest_failed_by_loop(loop_id, now_iso)?;
    Ok(())
}

struct StaleRunDecision {
    candidate: bool,
    uncertain: bool,
}

fn evaluate_stale_run_candidate(
    run: &RunRecord,
    active_executions: &[AgentExecutionRecord],
    _now: DateTime<Utc>,
    mode: &StaleRunReconcileMode,
) -> SchedulerResult<StaleRunDecision> {
    if *mode == StaleRunReconcileMode::Startup {
        let has_active_exec =
            active_executions.iter().any(|e| e.run_id.as_deref() == Some(&run.id) && e.status == "running");
        if !has_active_exec {
            return Ok(StaleRunDecision { candidate: true, uncertain: true });
        }
        return Ok(StaleRunDecision { candidate: true, uncertain: false });
    }

    let heartbeat_stale_threshold = chrono::Duration::minutes(30);
    let heartbeat_age = match &run.last_heartbeat_at {
        Some(hb) => match chrono::DateTime::parse_from_rfc3339(hb) {
            Ok(hb_dt) => {
                let hb_utc = hb_dt.with_timezone(&Utc);
                _now.signed_duration_since(hb_utc)
            }
            Err(_) => chrono::Duration::zero(),
        },
        None => match chrono::DateTime::parse_from_rfc3339(&run.started_at) {
            Ok(start_dt) => {
                let start_utc = start_dt.with_timezone(&Utc);
                _now.signed_duration_since(start_utc)
            }
            Err(_) => chrono::Duration::zero(),
        },
    };

    Ok(StaleRunDecision { candidate: heartbeat_age >= heartbeat_stale_threshold, uncertain: false })
}

#[allow(clippy::disallowed_methods)]
pub fn verify_process_identity(pid: u32, _expected_command: &str) -> Result<(bool, bool), String> {
    let output = Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .arg("-o")
        .arg("command=")
        .output()
        .map_err(|e| format!("failed to run ps: {e}"))?;

    if !output.status.success() {
        return Ok((false, false));
    }

    let actual_cmd = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if actual_cmd.is_empty() {
        return Ok((false, false));
    }

    let actual_first = actual_cmd.split_whitespace().next().unwrap_or("");
    let expected_first = _expected_command.split_whitespace().next().unwrap_or("");

    Ok((true, actual_first.contains(expected_first) || expected_first.contains(actual_first)))
}

#[allow(clippy::disallowed_methods)]
fn kill_process(pid: u32) -> Result<(), String> {
    let status = Command::new("kill").arg(pid.to_string()).status().map_err(|e| format!("failed to run kill: {e}"))?;

    if status.success() {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let check = Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("pid=")
            .output()
            .map_err(|e| format!("failed to check pid: {e}"))?;

        if !check.stdout.is_empty() {
            let kill9 = Command::new("kill")
                .arg("-9")
                .arg(pid.to_string())
                .status()
                .map_err(|e| format!("failed to run kill -9: {e}"))?;
            if !kill9.success() {
                return Err(format!("failed to kill -9 pid {pid}"));
            }
        }
        Ok(())
    } else {
        Err(format!("kill failed for pid {pid}"))
    }
}

fn format_javascript_iso_string(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S.000Z").to_string()
}

#[derive(Debug, Clone, Default)]
pub struct RecoverySummary {
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub orphan_cleaned: u64,
    pub orphan_uncertain: u64,
    pub expired_locks_released: u64,
    pub interrupted_runs: u64,
    pub loops_requeued: u64,
    pub queue_items_requeued: u64,
    pub queue_items_cancelled: u64,
    pub cleaned_executions: u64,
    pub skipped_uncertain_runs: u64,
    pub loop_normalization_events: u64,
    pub run_ids: Vec<String>,
    pub loop_ids: Vec<String>,
    pub execution_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_process_identity_nonexistent() {
        let (running, _) = verify_process_identity(99999999, "").unwrap_or((false, false));
        assert!(!running);
    }
}
