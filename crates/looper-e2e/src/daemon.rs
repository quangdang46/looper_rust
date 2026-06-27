//! Daemon lifecycle management for E2E tests.
//!
//! Ported from Go `legacy/internal/e2e/harness/daemon.go`.
//! Spawns `looperd` as a subprocess, waits for it to become ready,
//! and provides graceful shutdown.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use crate::binaries::BuiltBinaries;
use crate::temp_home::TempHome;

/// A running `looperd` daemon process for E2E tests.
#[allow(dead_code)]
pub struct DaemonProcess {
    child: Mutex<Option<Child>>,
    home: TempHome,
    config_path: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    base_url: String,
}

impl DaemonProcess {
    /// Base URL of the daemon's API server (e.g. `http://127.0.0.1:8080`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Path to the daemon's stdout log file.
    pub fn stdout_path(&self) -> &PathBuf {
        &self.stdout_path
    }

    /// Path to the daemon's stderr log file.
    pub fn stderr_path(&self) -> &PathBuf {
        &self.stderr_path
    }
}

/// Start a `looperd` daemon process for E2E testing.
///
/// Spawns the daemon with `--config <config_path>`, sets `HOME` to the temp
/// home directory, and captures stdout/stderr to log files in the artifacts
/// directory.  A cleanup handler is registered so the daemon is stopped
/// (with artifact dump) when the test completes or fails.
///
/// # Panics
/// Panics if the daemon binary cannot be started.
pub fn start_looperd(
    bins: &BuiltBinaries,
    home: &TempHome,
    config_path: &str,
    extra_env: &[(&str, &str)],
    host: &str,
    port: u16,
) -> DaemonProcess {
    let stdout_path = home.artifacts_dir.join("looperd.stdout.log");
    let stderr_path = home.artifacts_dir.join("looperd.stderr.log");

    let stdout_file = std::fs::File::create(&stdout_path).unwrap_or_else(|e| panic!("create looperd stdout log: {e}"));
    let stderr_file = std::fs::File::create(&stderr_path).unwrap_or_else(|e| panic!("create looperd stderr log: {e}"));

    let mut cmd = Command::new(&bins.looperd_path);
    cmd.arg("--config")
        .arg(config_path)
        .current_dir(&home.working_dir)
        .stdout(Stdio::from(stdout_file.try_clone().unwrap()))
        .stderr(Stdio::from(stderr_file.try_clone().unwrap()))
        .env("HOME", &home.home_dir);

    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let child = cmd.spawn().unwrap_or_else(|e| {
        panic!("start looperd (binary: {}) --config {}: {e}", bins.looperd_path.display(), config_path)
    });

    DaemonProcess {
        child: Mutex::new(Some(child)),
        home: home.clone(),
        config_path: PathBuf::from(config_path),
        stdout_path,
        stderr_path,
        base_url: crate::ports::base_url(host, port),
    }
}

impl DaemonProcess {
    /// Poll the daemon's `/api/v1/status` endpoint until it responds with
    /// HTTP 200 and a valid JSON envelope, or until `timeout` elapses.
    pub fn wait_for_ready(&self, timeout: Duration) -> Result<serde_json::Value, String> {
        let deadline = std::time::Instant::now() + timeout;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .map_err(|e| format!("build http client: {e}"))?;

        let status_url = format!("{}/health", self.base_url);

        loop {
            // Check if the daemon process exited prematurely
            {
                let mut guard = self.child.lock().unwrap();
                if let Some(ref mut child) = *guard {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            return Err(format!("looperd exited before readiness with status: {status}"));
                        }
                        Ok(None) => {} // still running
                        Err(e) => {
                            return Err(format!("looperd wait error: {e}"));
                        }
                    }
                }
            }

            if std::time::Instant::now() > deadline {
                return Err("timeout waiting for daemon to become ready".into());
            }

            match client.get(&status_url).send() {
                Ok(resp) if resp.status().is_success() => {
                    let body: serde_json::Value = resp.json().map_err(|e| format!("parse status response: {e}"))?;
                    // Check for expected "ok" field in envelope
                    if body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                        return Ok(body);
                    }
                }
                _ => {} // retry
            }

            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Stop the daemon gracefully.
    ///
    /// Sends `SIGTERM` and waits up to `timeout` for the process to exit.
    /// If the process doesn't exit in time, sends `SIGKILL`.
    pub fn stop(&self, timeout: Duration) {
        let mut guard = self.child.lock().unwrap();
        if let Some(mut child) = guard.take() {
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }

            let deadline = std::time::Instant::now() + timeout;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {}
                    Err(_) => break,
                }
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    /// Read and return the daemon's stdout log content.
    pub fn read_stdout(&self) -> String {
        std::fs::read_to_string(&self.stdout_path).unwrap_or_default()
    }

    /// Read and return the daemon's stderr log content.
    pub fn read_stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.stop(Duration::from_secs(5));
    }
}

// Re-export testing context for the start_looperd function signature
pub mod testing {
    /// Minimal test context placeholder mirroring Go's `testing.TB`.
    /// In real usage, tests use `#[test]` and harness functions directly.
    pub struct TestContext;
}
