//! Agent process cleanup — kill stale Claude Code processes and recover
//! orphaned executions from the database on daemon startup.
//!
//! Ported from Go internal/runtime/runtime.go runRecoveryPipeline.

use std::process::Command;
use std::sync::Arc;

use chrono::Utc;
use looper_storage::record::{AgentExecutionRecord, RunRecord};
use looper_storage::Repositories;

/// Kill any looper-spawned Claude processes that aren't the current session.
/// Called during daemon startup to clean up after a crash.
pub fn kill_stale_agent_processes() {
    match Command::new("pkill")
        .args(["-f", "claude.*--dangerously-skip-permissions"])
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                tracing::info!("Killed stale agent process(es): {}", stdout.trim());
            }
        }
        Err(e) => {
            tracing::warn!("Failed to kill stale agents: {e}");
        }
    }
}

/// Recover orphaned agent executions from the DB on startup.
///
/// Lists all active (running) agent executions and marks them recovered.
/// Sends SIGTERM then SIGKILL to the recorded PID if available.
/// Ported from Go looper's runRecoveryPipeline.
pub fn recover_orphan_executions(repos: &Arc<Repositories>) {
    let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let cutoff = (Utc::now() - chrono::Duration::hours(24))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();

    let active = match repos.agent_executions.list_active() {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("Failed to list active executions for recovery: {e}");
            return;
        }
    };

    let mut recovered = 0u32;
    for exec in &active {
        if exec.created_at < cutoff {
            let mut updated = exec.clone();
            updated.status = "recovered".to_string();
            updated.updated_at.clone_from(&now_iso);
            let _ = repos.agent_executions.upsert(&updated);
            recovered += 1;
            continue;
        }

        if let Some(pid) = extract_pid_from_execution(exec) {
            let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).output();
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).output();
        }

        let mut updated = exec.clone();
        updated.status = "recovered".to_string();
        updated.updated_at.clone_from(&now_iso);
        let _ = repos.agent_executions.upsert(&updated);
        recovered += 1;
    }

    if recovered > 0 {
        tracing::info!("Recovered {recovered} orphaned agent execution(s) on startup");
    }
}

/// Extract a PID from an execution record's metadata_json.
fn extract_pid_from_execution(exec: &AgentExecutionRecord) -> Option<i32> {
    if let Some(ref meta) = exec.metadata_json {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(meta) {
            if let Some(pid) = val.get("pid").and_then(|v| v.as_i64()) {
                return Some(pid as i32);
            }
        }
    }
    None
}

/// Check if a run is stale based on heartbeat age.
pub fn is_stale_run(run: &RunRecord, stale_threshold_secs: i64) -> bool {
    match run.last_heartbeat_at.as_ref() {
        Some(hb) => match chrono::DateTime::parse_from_rfc3339(hb) {
            Ok(t) => {
                let t_utc = t.with_timezone(&Utc);
                let age = (Utc::now() - t_utc).num_seconds();
                age > stale_threshold_secs
            }
            Err(_) => true,
        },
        None => true,
    }
}

/// Interrupt stale runs by marking them as interrupted.
pub fn interrupt_stale_runs(repos: &Arc<Repositories>, stale_threshold_secs: i64) -> u32 {
    let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let mut interrupted = 0u32;

    let running_runs = match repos.runs.list_by_status("running") {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Failed to list running runs: {e}");
            return 0;
        }
    };

    for run in &running_runs {
        if is_stale_run(run, stale_threshold_secs) {
            let mut updated = run.clone();
            updated.status = "interrupted".to_string();
            updated.ended_at = Some(now_iso.clone());
            if let Err(e) = repos.runs.upsert(&updated) {
                tracing::warn!("Failed to interrupt stale run {}: {e}", run.id);
            } else {
                tracing::info!("Interrupted stale run {} (no heartbeat)", run.id);
                interrupted += 1;
            }
        }
    }
    interrupted
}
