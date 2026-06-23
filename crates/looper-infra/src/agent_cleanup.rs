use std::process::Command;

pub fn kill_stale_agent_processes() {
    // Find and kill stale claude processes spawned by looper
    // These may survive a daemon crash and eat memory
    if let Ok(output) = Command::new("pkill").args(["-f", "claude.*--dangerously-skip-permissions"]).output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            tracing::info!("Killed stale agent process(es): {}", stdout.trim());
        }
    }
}
