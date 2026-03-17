use anyhow::{anyhow, Context, Result};
use camino::Utf8PathBuf;
use std::{
    io::{BufRead, BufReader, Lines},
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartSessionRequest {
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
pub struct CliClaudeBackend {
    claude_bin: String,
}

impl CliClaudeBackend {
    #[must_use]
    pub fn new(claude_bin: impl Into<String>) -> Self {
        Self {
            claude_bin: claude_bin.into(),
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

impl ClaudeBackend for CliClaudeBackend {
    fn start(&self, req: StartSessionRequest) -> Result<RunningSession> {
        let mut command = Command::new(&self.claude_bin);
        command
            .arg("-p")
            .arg(&req.prompt)
            .arg("--model")
            .arg(&req.model)
            .current_dir(req.working_dir.as_std_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (key, value) in &req.env {
            command.env(key, value);
        }

        let mut child = command.spawn().with_context(|| {
            format!("spawn {} in {}", self.claude_bin, req.working_dir.as_str())
        })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("{} did not expose stdout pipe", self.claude_bin))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("{} did not expose stderr pipe", self.claude_bin))?;

        Ok(RunningSession {
            child,
            stdout: BufReader::new(stdout).lines(),
            stderr: BufReader::new(stderr).lines(),
            timeout: req.timeout,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ClaudeBackend, CliClaudeBackend, StartSessionRequest};
    use camino::Utf8PathBuf;
    use std::{error::Error, fs, time::Duration};
    use tempfile::tempdir;

    type TestResult = Result<(), Box<dyn Error>>;

    #[cfg(unix)]
    fn write_fake_claude_script(path: &std::path::Path) -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let script = r#"#!/bin/sh
printf '%s\n' "$@" > "$ARGS_FILE"
printf '%s' "$TEST_TOKEN" > "$ENV_FILE"
pwd > "$PWD_FILE"
printf 'stdout line\n'
printf 'stderr line\n' >&2
"#;
        fs::write(path, script)?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn cli_backend_spawns_process_with_expected_contract() -> TestResult {
        let dir = tempdir()?;
        let workspace_dir = dir.path().join("workspace");
        fs::create_dir_all(&workspace_dir)?;

        let script_path = dir.path().join("fake-claude");
        write_fake_claude_script(&script_path)?;

        let args_file = dir.path().join("args.txt");
        let env_file = dir.path().join("env.txt");
        let pwd_file = dir.path().join("pwd.txt");

        let working_dir = Utf8PathBuf::from_path_buf(workspace_dir.clone())
            .map_err(|_| std::io::Error::other("workspace dir must be valid UTF-8"))?;
        let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());
        let mut session = backend.start(StartSessionRequest {
            model: "sonnet".to_owned(),
            prompt: "write code while you sleep".to_owned(),
            working_dir,
            timeout: Duration::from_secs(90),
            env: vec![
                (
                    "ARGS_FILE".to_owned(),
                    args_file.to_string_lossy().into_owned(),
                ),
                (
                    "ENV_FILE".to_owned(),
                    env_file.to_string_lossy().into_owned(),
                ),
                (
                    "PWD_FILE".to_owned(),
                    pwd_file.to_string_lossy().into_owned(),
                ),
                ("TEST_TOKEN".to_owned(), "backend-env".to_owned()),
            ],
        })?;

        let stdout_line = session
            .stdout
            .next()
            .transpose()?
            .ok_or_else(|| std::io::Error::other("missing stdout line"))?;
        let stderr_line = session
            .stderr
            .next()
            .transpose()?
            .ok_or_else(|| std::io::Error::other("missing stderr line"))?;
        let status = session.child.wait()?;

        assert!(status.success());
        assert_eq!(stdout_line, "stdout line");
        assert_eq!(stderr_line, "stderr line");
        assert_eq!(session.timeout(), Duration::from_secs(90));

        let recorded_args = fs::read_to_string(&args_file)?;
        let args: Vec<_> = recorded_args.lines().collect();
        assert_eq!(
            args,
            vec!["-p", "write code while you sleep", "--model", "sonnet"]
        );
        assert_eq!(fs::read_to_string(&env_file)?, "backend-env");
        assert_eq!(
            fs::read_to_string(&pwd_file)?.trim(),
            workspace_dir.display().to_string()
        );
        Ok(())
    }

    #[test]
    fn cli_backend_surfaces_spawn_failures() {
        let backend = CliClaudeBackend::new("/definitely/missing/claude");
        let request = StartSessionRequest {
            model: "sonnet".to_owned(),
            prompt: "hello".to_owned(),
            working_dir: Utf8PathBuf::from("."),
            timeout: Duration::from_secs(30),
            env: Vec::new(),
        };

        let result = backend.start(request);
        assert!(result.is_err());
        if let Err(error) = result {
            assert!(error
                .to_string()
                .contains("spawn /definitely/missing/claude in ."));
        }
    }
}
