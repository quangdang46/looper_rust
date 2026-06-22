use std::path::PathBuf;
use std::process::Stdio;

use crate::error::CliError;

/// Detect the looperd binary path (same dir as looper-cli, or in PATH).
fn daemon_binary() -> Result<PathBuf, CliError> {
    // First check same directory as current executable
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.parent().map(|p| p.join("looperd"));
        if let Some(path) = sibling {
            if path.exists() {
                return Ok(path);
            }
        }
    }
    // Fall back to PATH
    which::which("looperd").map_err(|_| {
        CliError::daemon_lifecycle("looperd binary not found (not in PATH and not alongside looper-cli)")
    })
}

/// Default log file path.
fn log_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local").join("share").join("looper").join("daemon.log")
}

/// Start the daemon in the background.
#[allow(clippy::disallowed_methods)]
pub async fn start() -> Result<(), CliError> {
    let bin = daemon_binary()?;
    let log = log_file();

    // Create log directory
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_file = log.clone();
    let child = std::process::Command::new(&bin)
        .stdout(Stdio::from(
            std::fs::File::create(&log).map_err(|e| {
                CliError::daemon_lifecycle(format!("cannot create log file {log:?}: {e}"))
            })?,
        ))
        .stderr(Stdio::inherit())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| CliError::daemon_lifecycle(format!("cannot start daemon: {e}")))?;

    println!("looperd started (PID {}), log: {}", child.id(), log_file.display());
    Ok(())
}

/// Stop the daemon by finding and killing the looperd process.
#[allow(clippy::disallowed_methods)]
pub async fn stop() -> Result<(), CliError> {
    let output = std::process::Command::new("pkill")
        .arg("-f")
        .arg("looperd")
        .output()
        .map_err(|e| CliError::daemon_lifecycle(format!("cannot stop daemon: {e}")))?;

    if output.status.success() {
        println!("looperd stopped");
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not found") || stderr.contains("no process") {
            println!("looperd is not running");
        } else {
            return Err(CliError::daemon_lifecycle(
                format!("failed to stop daemon: {stderr}"),
            ));
        }
    }
    Ok(())
}

/// Restart the daemon.
pub async fn restart() -> Result<(), CliError> {
    stop().await?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    start().await
}

/// Print daemon status (check if looperd process is running).
#[allow(clippy::disallowed_methods)]
pub async fn status() -> Result<(), CliError> {
    let output = std::process::Command::new("pgrep")
        .arg("-f")
        .arg("looperd")
        .output()
        .map_err(|e| CliError::daemon_lifecycle(format!("cannot check status: {e}")))?;

    if output.status.success() {
        let pid_str = String::from_utf8_lossy(&output.stdout);
        let pid = pid_str.trim();
        println!("looperd is running (PID {pid})");
    } else {
        println!("looperd is not running");
    }
    Ok(())
}

/// Tail the daemon log file.
#[allow(clippy::disallowed_methods)]
pub async fn logs(lines: Option<usize>) -> Result<(), CliError> {
    let log = log_file();
    if !log.exists() {
        return Err(CliError::daemon_lifecycle(format!("log file not found at {}", log.display())));
    }
    let n = lines.unwrap_or(50).to_string();
    let status = std::process::Command::new("tail")
        .arg("-n")
        .arg(&n)
        .arg(&log)
        .status()
        .map_err(|e| CliError::daemon_lifecycle(format!("cannot tail log: {e}")))?;
    if !status.success() {
        return Err(CliError::daemon_lifecycle("tail command failed"));
    }
    Ok(())
}
