//! Agent process cleanup — kill stale Claude Code processes and recover
//! orphaned executions from the database on daemon startup.
//!
//! Ported from Go internal/runtime/runtime.go runRecoveryPipeline.

use std::process::Command;
use std::sync::Arc;

use chrono::Utc;
use looper_storage::record::{AgentExecutionRecord, RunRecord};
use looper_storage::Repositories;

/// Kill any looper-spawned agent processes still running.
///
/// Uses `kill` (or `killpg`) on recorded PIDs from agent_executions
/// that are still in running/killed status. Only kills processes that
/// looper explicitly recorded — never uses pkill.
///
/// On daemon startup this runs before the DB is opened so it's a no-op;
/// the actual recovery uses `recover_orphan_executions()` below.
pub fn kill_stale_agent_processes() {
    tracing::debug!("kill_stale_agent_processes called before DB open — deferring to recover_orphan_executions");
}

/// Recover orphaned agent executions from the DB on startup.
///
/// Lists all active (running) agent executions and marks them recovered.
/// Sends SIGTERM then SIGKILL to the recorded process group if available,
/// NOT just the PID — this ensures child processes are also terminated.
/// Ported from Go looper's runRecoveryPipeline.
pub fn recover_orphan_executions(repos: &Arc<Repositories>) {
    let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    let active = match repos.agent_executions.list_active() {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("Failed to list active executions for recovery: {e}");
            return;
        }
    };

    let mut recovered = 0u32;
    for exec in &active {
        // Kill by process group first (kills children too), then by PID as fallback
        let pgid = extract_pgid_from_execution(exec);
        let pid = extract_pid_from_execution(exec);

        if let Some(gid) = pgid {
            // Kill entire process group — catches children
            let _ = Command::new("kill").args(["-TERM", &format!("-{gid}")]).output();
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = Command::new("kill").args(["-KILL", &format!("-{gid}")]).output();
            tracing::debug!("Killed orphan process group {gid}");
        } else if let Some(p) = pid {
            // Fallback: kill just the PID (children may survive)
            let _ = Command::new("kill").args(["-TERM", &p.to_string()]).output();
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = Command::new("kill").args(["-KILL", &p.to_string()]).output();
            tracing::debug!("Killed orphan PID {p} (no PGID recorded)");
        }

        // Also try to kill any remaining looper-spawned claude processes by matching
        // the working directory from the execution metadata.
        kill_orphan_by_workdir(exec);

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

/// Kill any orphaned agent processes that match the execution's CWD.
fn kill_orphan_by_workdir(exec: &AgentExecutionRecord) {
    // The cwd is stored in the AgentExecutionRecord directly
    let wd = match &exec.cwd {
        Some(w) => w.clone(),
        None => return,
    };

    // Scan running processes for one matching the CWD
    if let Ok(out) = Command::new("ps").args(["-eo", "pid=,args="]).output() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.contains(&wd) {
                if let Some(pid_str) = line.split_whitespace().next() {
                    if let Ok(pid) = pid_str.parse::<i32>() {
                        // Only kill processes that look like looper agents (claude/codex/opencode)
                        let lower = line.to_lowercase();
                        if lower.contains("claude") || lower.contains("codex") || lower.contains("opencode") {
                            let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).output();
                            tracing::info!("Killed orphan agent PID={} in workdir {}", pid, wd);
                        }
                    }
                }
            }
        }
    }
}

/// Extract a process group ID from the execution record's metadata_json.
/// Looper's executor stores PGID in metadata as `process_group` or `pgid`.
fn extract_pgid_from_execution(exec: &AgentExecutionRecord) -> Option<i32> {
    if let Some(ref meta) = exec.metadata_json {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(meta) {
            // Try process_group first, then pgid, then fall back to pid
            if let Some(gid) = val.get("process_group").and_then(|v| v.as_i64()) {
                return Some(gid as i32);
            }
            if let Some(gid) = val.get("pgid").and_then(|v| v.as_i64()) {
                return Some(gid as i32);
            }
        }
    }
    None
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
