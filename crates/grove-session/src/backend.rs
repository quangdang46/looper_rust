#![allow(clippy::unwrap_used, clippy::expect_used)]
use anyhow::{Context, Result, anyhow};
use camino::Utf8PathBuf;
use grove_types::RuntimeProvider;
use std::{
    io::{BufRead, BufReader, Lines},
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartSessionRequest {
    pub provider: RuntimeProvider,
    pub model: String,
    pub prompt: String,
    pub working_dir: Utf8PathBuf,
    pub timeout: Duration,
    pub env: Vec<(String, String)>,
}

pub trait ClaudeBackend: Send + Sync {
    fn start(&self, req: StartSessionRequest) -> Result<RunningSession>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliSessionBackend {
    provider: RuntimeProvider,
    provider_bin: String,
}

pub trait SessionBackend: ClaudeBackend {}

impl<T: ClaudeBackend + ?Sized> SessionBackend for T {}

pub type CliClaudeBackend = CliSessionBackend;

impl CliSessionBackend {
    #[must_use]
    pub fn new(provider_bin: impl Into<String>) -> Self {
        Self::new_for_provider(RuntimeProvider::Claude, provider_bin)
    }

    #[must_use]
    pub fn new_for_provider(provider: RuntimeProvider, provider_bin: impl Into<String>) -> Self {
        Self {
            provider,
            provider_bin: provider_bin.into(),
        }
    }
}

pub struct RunningSession {
    pub child: Child,
    pub stdout: Lines<BufReader<ChildStdout>>,
    pub stderr: Lines<BufReader<ChildStderr>>,
    timeout: Duration,
}

impl RunningSession {
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// Sentinel: when `default_model` in config is `"default"`, Grove does not pass `--model`
/// so the provider CLI uses its own default model.
pub const DEFAULT_MODEL_OMIT_FLAG: &str = "default";

impl ClaudeBackend for CliSessionBackend {
    fn start(&self, req: StartSessionRequest) -> Result<RunningSession> {
        let provider = req.provider;
        let mut command = Command::new(&self.provider_bin);
        match provider {
            RuntimeProvider::Claude => {
                command
                    .arg("-p")
                    .arg(&req.prompt)
                    .arg("--permission-mode")
                    .arg("bypassPermissions");
                if req.model != DEFAULT_MODEL_OMIT_FLAG {
                    command.arg("--model").arg(&req.model);
                }
            }
            RuntimeProvider::Codex => {
                command.arg("exec").arg("--full-auto");
                if req.model != DEFAULT_MODEL_OMIT_FLAG {
                    command.arg("--model").arg(&req.model);
                }
                command.arg(&req.prompt);
            }
        }
        command
            .current_dir(req.working_dir.as_std_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (key, value) in &req.env {
            command.env(key, value);
        }

        let mut child = command.spawn().with_context(|| {
            format!(
                "spawn {} in {}",
                self.provider_bin,
                req.working_dir.as_str()
            )
        })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("{} did not expose stdout pipe", self.provider_bin))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("{} did not expose stderr pipe", self.provider_bin))?;

        Ok(RunningSession {
            child,
            stdout: BufReader::new(stdout).lines(),
            stderr: BufReader::new(stderr).lines(),
            timeout: req.timeout,
        })
    }
}

#[cfg(all(test, unix))]
mod tests;
