//! Shell command execution utilities.
//!
//! Provides simple, reusable free functions for running shell commands,
//! capturing output, and enforcing timeouts. Consolidates patterns
//! duplicated across gateway (`run_gh_command`), bootstrap
//! (`validate_tools`), and agent executor (`cmd.spawn()`).

use std::process::Command;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Captured output from a shell command.
#[derive(Debug, Clone, Default)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run a shell command and capture its output.
///
/// Equivalent to `run_command_with_timeout(cmd, args, cwd, Duration::from_secs(120))`.
pub fn run_command(cmd: &str, args: &[&str], cwd: &str) -> Result<CommandResult, String> {
    run_command_with_timeout(cmd, args, cwd, Duration::from_secs(120))
}

/// Run a shell command with a specified timeout.
///
/// The timeout is enforced via a background thread join — if the command
/// does not complete within the timeout, the handle is dropped (leaving
/// the child process orphaned — no SIGKILL is sent, as `std::process`
/// does not provide portable child-process termination).
pub fn run_command_with_timeout(
    cmd: &str,
    args: &[&str],
    cwd: &str,
    timeout: Duration,
) -> Result<CommandResult, String> {
    let cmd_str = cmd.to_string();
    let cmd_for_err = cmd_str.clone();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let cwd = cwd.to_string();
    let handle = std::thread::spawn(move || {
        let output = Command::new(&cmd_str)
            .args(&args)
            .current_dir(&cwd)
            .output()
            .map_err(|e| format!("spawn {cmd_str}: {e}"))?;

        Ok(CommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    });

    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(format!("command timed out after {timeout:?}: {cmd_for_err}")),
    }
}

/// Run a shell command with stdin provided.
pub fn run_command_with_stdin(
    cmd: &str,
    args: &[&str],
    stdin: &str,
    cwd: &str,
) -> Result<CommandResult, String> {
    let mut child = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {cmd}: {e}"))?;

    use std::io::Write;
    if let Some(ref mut stdin_writer) = child.stdin {
        let _ = stdin_writer.write_all(stdin.as_bytes());
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait {cmd}: {e}"))?;

    Ok(CommandResult {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_command_echo() {
        let result = run_command("echo", &["hello world"], ".").unwrap();
        assert_eq!(result.stdout.trim(), "hello world");
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn test_run_command_nonzero_exit() {
        let result = run_command("sh", &["-c", "exit 42"], ".").unwrap();
        assert_eq!(result.exit_code, 42);
    }

    #[test]
    fn test_run_command_binary_not_found() {
        let result = run_command("nonexistent-binary-12345", &[], ".");
        assert!(result.is_err());
    }
}
