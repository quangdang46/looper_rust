//! Agent executor — process lifecycle, two-tier timeout, signal escalation.

use crate::args::{append_completion_instruction_to, resolve_spawn_with_native_resume};
use crate::args::resolve_spawn;
use crate::env::build_command_env;
use crate::error::AgentError;
use crate::parse::{extract_native_session_id, parse_completion};
use crate::types::{
    AgentResult, CompletionParseStatus, ExecutorConfig, ExecutionState,
    NativeResumeMode, NativeResumeStatus, RunInput, SpawnCommand, TimeoutType, COMPLETION_MARKER,
};
use looper_storage::record::AgentExecutionRecord;
use looper_storage::Repositories;
use nix::sys::signal::{killpg, Signal};
use nix::unistd::{Pid, setpgid};
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Input for starting a new agent session in a runner step pipeline.
#[derive(Default)]
pub struct StartInput {
    pub loop_id: String,
    pub current_step: Option<String>,
    pub last_completed_step: Option<String>,
    pub checkpoint_json: Option<String>,
    pub project_id: String,
    pub run_id: String,
    /// Working directory for the agent (typically the worktree path).
    pub working_directory: String,
    /// Prompt describing what the agent should do.
    pub prompt: String,
}

impl From<StartInput> for RunInput {
    fn from(input: StartInput) -> Self {
        Self {
            loop_id: input.loop_id,
            project_id: input.project_id,
            run_id: input.run_id,
            working_directory: input.working_directory,
            prompt: input.prompt,
            ..Default::default()
        }
    }
}

/// The main executor — creates and manages agent executions.
pub struct ConfiguredExecutor {
    pub config: ExecutorConfig,
    pub repos: Arc<std::sync::Mutex<Repositories>>,
    pub log_dir: String,
    pub now: fn() -> chrono::DateTime<chrono::Utc>,
}

impl ConfiguredExecutor {
    pub fn new(
        config: ExecutorConfig,
        repos: Arc<std::sync::Mutex<Repositories>>,
        log_dir: String,
        now: fn() -> chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            config,
            repos,
            log_dir,
            now,
        }
    }

    /// Start a new execution and return a handle.
    pub async fn start(&self, input: impl Into<RunInput>) -> Result<Execution, AgentError> {
        let input = input.into();
        if input.prompt.is_empty() {
            return Err(AgentError::InvalidInput("prompt is required".into()));
        }
        if input.working_directory.is_empty() {
            return Err(AgentError::InvalidInput(
                "working_directory is required".into(),
            ));
        }

        let prompt = append_completion_instruction(&input.prompt);
        let native_resume_prompt = input
            .native_resume_prompt
            .as_ref()
            .map(|p| append_completion_instruction(p));

        let (spawn_cmd, resume_mode, resume_status, native_session_id) = self
            .resolve_spawn_with_resume(&input, &prompt, native_resume_prompt.as_deref())
            .await?;

        let env = build_command_env(&input.working_directory, &prompt, &[&self.config.env, &input.env]);

        let log_dir_path = std::path::Path::new(&self.log_dir)
            .join("loops")
            .join(&input.loop_id)
            .join(&input.run_id);
        tokio::fs::create_dir_all(&log_dir_path).await.map_err(AgentError::Io)?;

        let stdout_log = log_dir_path.join(format!("{}.stdout.log", input.execution_id));
        let stderr_log = log_dir_path.join(format!("{}.stderr.log", input.execution_id));

        let mut cmd = Command::new(&spawn_cmd.binary);
        cmd.args(&spawn_cmd.args)
            .env_clear()
            .envs(env.iter().filter_map(|e| {
                let mut parts = e.splitn(2, '=');
                let key = parts.next()?;
                let val = parts.next().unwrap_or("");
                Some((key, val))
            }))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(&input.working_directory);

        // SAFETY: pre_exec runs in the child process after fork, before exec.
        // Creating a new process group here allows clean SIGTERM/SIGKILL escalation
        // to all child processes.
        unsafe {
            cmd.as_std_mut().pre_exec(|| {
                setpgid(Pid::from_raw(0), Pid::from_raw(0)).ok();
                Ok(())
            });
        }

        // Ensure working directory exists before spawning; on macOS a
        // missing cwd produces ErrorKind::NotFound which is indistinguishable
        // from a missing binary, so we check first.
        if !std::path::Path::new(&input.working_directory).exists() {
            tokio::fs::create_dir_all(&input.working_directory).await.map_err(AgentError::Io)?;
        }

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AgentError::CommandNotFound(spawn_cmd.binary.clone())
            } else {
                AgentError::Io(e)
            }
        })?;

        let pid = child.id().ok_or_else(|| AgentError::Other("no PID assigned".into()))?;

        let stdout = child.stdout.take().ok_or_else(|| {
            AgentError::Other("failed to capture stdout".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AgentError::Other("failed to capture stderr".to_string())
        })?;

        let now = (self.now)();
        let state = Arc::new(Mutex::new(ExecutionState {
            pid: Some(pid),
            start_time: now,
            last_output_time: now,
            stdout_buffer: Vec::with_capacity(input.max_output_bytes + 1024),
            stderr_buffer: Vec::with_capacity(input.max_output_bytes + 1024),
            timed_out: false,
            killed: false,
            completed: false,
            timeout_type: None,
            heartbeat_count: 0,
            native_session_id: native_session_id.clone(),
        }));

        let killed_flag = Arc::new(AtomicBool::new(false));

        let stdout_log_file = tokio::fs::File::create(&stdout_log).await.map_err(AgentError::Io)?;
        let stderr_log_file = tokio::fs::File::create(&stderr_log).await.map_err(AgentError::Io)?;

        let execution_id = input.execution_id.clone();
        let vendor_str = self.config.vendor.as_str().to_string();
        let cwd = input.working_directory.clone();

        let command_json = serde_json::json!({
            "command": spawn_cmd.binary,
            "args": spawn_cmd.args,
        })
        .to_string();

        let metadata = serde_json::json!({
            "idempotencyKey": input.idempotency_key,
            "timeoutPolicy": {
                "maxRuntime": input.timeout.as_secs(),
                "idleTimeout": input.heartbeat_timeout.as_secs(),
            },
        })
        .to_string();

        let record = AgentExecutionRecord {
            id: execution_id.clone(),
            project_id: Some(input.project_id.clone()),
            loop_id: Some(input.loop_id.clone()),
            run_id: Some(input.run_id.clone()),
            vendor: vendor_str,
            status: "running".to_string(),
            pid: Some(pid as i64),
            command_json: Some(command_json),
            cwd: Some(cwd),
            summary: None,
            parse_status: None,
            completion_signal: None,
            output_json: Some(serde_json::json!({
                "stdoutLogPath": stdout_log.to_string_lossy().to_string(),
                "stderrLogPath": stderr_log.to_string_lossy().to_string(),
            }).to_string()),
            error_message: None,
            native_session_id: native_session_id.clone(),
            native_resume_mode: resume_mode.map(|m| m.as_str().to_string()),
            native_resume_status: resume_status.map(|m| m.as_str().to_string()),
            native_resume_error: None,
            heartbeat_count: 0,
            last_heartbeat_at: None,
            started_at: now.to_rfc3339(),
            ended_at: None,
            metadata_json: Some(metadata),
            created_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
        };

        self.repos.lock().expect("agent repo lock poisoned").agent_executions.upsert(&record).map_err(AgentError::Storage)?;

        let execution = Execution {
            child: Arc::new(Mutex::new(Some(child))),
            state: state.clone(),
            killed_flag,
            stdout_reader: Arc::new(Mutex::new(Some(stdout))),
            stderr_reader: Arc::new(Mutex::new(Some(stderr))),
            stdout_log: Arc::new(Mutex::new(Some(stdout_log_file))),
            stderr_log: Arc::new(Mutex::new(Some(stderr_log_file))),
            result: Arc::new(Mutex::new(None)),
            done: Arc::new(Mutex::new(false)),
            config: self.config.clone(),
            input: input.clone(),
            execution_id,
            max_output_bytes: input.max_output_bytes,
        };

        Ok(execution)
    }

    /// Persist execution result to the database.
    pub async fn persist_result(
        &self,
        execution_id: &str,
        result: &AgentResult,
        native_session_id: Option<String>,
        error_msg: Option<String>,
        parse_status: &CompletionParseStatus,
    ) {
        let now = chrono::Utc::now();
        let record = AgentExecutionRecord {
            id: execution_id.to_string(),
            status: result.status.clone(),
            summary: Some(result.summary.clone()),
            parse_status: Some(parse_status.as_str().to_string()),
            completion_signal: result.completion_signal.clone(),
            error_message: error_msg,
            native_session_id,
            heartbeat_count: result.heartbeat_count,
            last_heartbeat_at: result.last_progress_at.clone(),
            ended_at: Some(now.to_rfc3339()),
            updated_at: now.to_rfc3339(),
            ..default_execution_record()
        };
        let _ = self.repos.lock().map(|g| g.agent_executions.upsert(&record));
    }

    /// Persist killed status to the database.
    pub async fn persist_kill(&self, execution_id: &str, reason: &str) {
        let now = chrono::Utc::now();
        let record = AgentExecutionRecord {
            id: execution_id.to_string(),
            status: "cancelling".to_string(),
            updated_at: now.to_rfc3339(),
            ..default_execution_record()
        };
        let _ = self.repos.lock().map(|g| g.agent_executions.upsert(&record));
        tracing::info!(
            "Persisted kill status for execution {} (reason: {})",
            execution_id,
            reason
        );
    }

    /// Resolve spawn command with native resume consideration.
    async fn resolve_spawn_with_resume(
        &self,
        input: &RunInput,
        prompt: &str,
        native_resume_prompt: Option<&str>,
    ) -> Result<
        (
            SpawnCommand,
            Option<NativeResumeMode>,
            Option<NativeResumeStatus>,
            Option<String>,
        ),
        AgentError,
    > {
        if !self.config.native_resume_enabled {
            return Ok((
                resolve_spawn(&self.config, &input.working_directory, prompt),
                Some(NativeResumeMode::CheckpointRestart),
                Some(NativeResumeStatus::Disabled),
                None,
            ));
        }

        if !self.config.vendor.native_resume_supported() {
            return Ok((
                resolve_spawn(&self.config, &input.working_directory, prompt),
                Some(NativeResumeMode::CheckpointRestart),
                Some(NativeResumeStatus::Unsupported),
                None,
            ));
        }

        let session_id = if let Some(ref sid) = input.native_session_id {
            Some(sid.clone())
        } else {
            self.find_resumable_session(&input.loop_id).await?
        };

        if let Some(ref sid) = session_id {
            let resume_prompt = native_resume_prompt.unwrap_or(prompt);
            let cmd = resolve_spawn_with_native_resume(
                &self.config,
                &input.working_directory,
                sid,
                resume_prompt,
            );
            Ok((
                cmd,
                Some(NativeResumeMode::NativeResume),
                Some(NativeResumeStatus::Started),
                Some(sid.clone()),
            ))
        } else {
            Ok((
                resolve_spawn(&self.config, &input.working_directory, prompt),
                Some(NativeResumeMode::CheckpointRestart),
                Some(NativeResumeStatus::Unavailable),
                None,
            ))
        }
    }

    /// Find a resumable session from the latest agent execution for a loop.
    async fn find_resumable_session(
        &self,
        loop_id: &str,
    ) -> Result<Option<String>, AgentError> {
        let latest = self
            .repos
            .lock()
            .expect("agent repo lock poisoned")
            .agent_executions
            .get_latest_by_loop_id(loop_id)
            .map_err(AgentError::Storage)?;

        let latest = match latest {
            Some(e) => e,
            None => return Ok(None),
        };

        let valid_statuses = [
            "running", "cancelling", "killed", "timeout", "failed", "completed",
        ];

        let vendor_matches = latest.vendor == self.config.vendor.as_str();
        let has_session = latest.native_session_id.is_some();
        let resume_pending = latest.native_resume_status.as_deref() == Some("pending");
        let status_ok = valid_statuses.contains(&latest.status.as_str());

        if vendor_matches && has_session && resume_pending && status_ok {
            Ok(latest.native_session_id.clone())
        } else {
            Ok(None)
        }
    }
}

/// A running execution handle. Does NOT contain Repositories — all DB persistence
/// is handled by `ConfiguredExecutor` to avoid `!Send` issues with rusqlite::Connection.
#[derive(Clone)]
#[allow(dead_code)]
pub struct Execution {
    child: Arc<Mutex<Option<Child>>>,
    state: Arc<Mutex<ExecutionState>>,
    killed_flag: Arc<AtomicBool>,
    stdout_reader: Arc<Mutex<Option<tokio::process::ChildStdout>>>,
    stderr_reader: Arc<Mutex<Option<tokio::process::ChildStderr>>>,
    stdout_log: Arc<Mutex<Option<tokio::fs::File>>>,
    stderr_log: Arc<Mutex<Option<tokio::fs::File>>>,
    result: Arc<Mutex<Option<AgentResult>>>,
    done: Arc<Mutex<bool>>,
    config: ExecutorConfig,
    input: RunInput,
    execution_id: String,
    max_output_bytes: usize,
}

impl Execution {
    /// Wait for the execution to complete and return the result.
    pub async fn wait(&self) -> Result<AgentResult, AgentError> {
        loop {
            {
                let done = self.done.lock().await;
                if *done {
                    let result_opt = self.result.lock().await;
                    if let Some(ref result) = *result_opt {
                        return Ok(result.clone());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Kill the execution with a reason.
    pub async fn kill(&self, reason: &str) -> Result<(), AgentError> {
        self.killed_flag.store(true, Ordering::SeqCst);
        let pid = {
            let state = self.state.lock().await;
            state.pid
        };

        if let Some(pid) = pid {
            let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGTERM);
            tracing::info!("Sent SIGTERM to process group {} (reason: {})", pid, reason);
        }

        Ok(())
    }

    /// Run the main execution loop. This does NOT persist to DB —
    /// the caller (ConfiguredExecutor::spawn_and_persist or the runtime)
    /// handles persistence after this returns.
    pub async fn run_loop(self) -> AgentResult {
        let max_runtime = self.input.timeout;
        let heartbeat_timeout = self.input.heartbeat_timeout;
        let graceful_shutdown = self.input.graceful_shutdown;
        let max_output = self.max_output_bytes;

        let start = Instant::now();
        let mut max_runtime_fired = false;
        let mut idle_fired = false;

        let stdout_reader = self.stdout_reader.clone();
        let stderr_reader = self.stderr_reader.clone();
        let state = self.state.clone();
        let result = self.result.clone();
        let done = self.done.clone();
        let killed_flag = self.killed_flag.clone();
        let _child_lock = self.child.clone();

        let state_stdout = state.clone();
        let stdout_handle = tokio::spawn(async move {
            let mut reader = stdout_reader.lock().await;
            if let Some(ref mut r) = *reader {
                let mut buf = [0u8; 8192];
                loop {
                    match r.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut s = state_stdout.lock().await;
                            if s.stdout_buffer.len() < max_output {
                                s.stdout_buffer.extend_from_slice(&buf[..n]);
                            }
                            s.last_output_time = chrono::Utc::now();
                            s.heartbeat_count += 1;
                        }
                        Err(_) => break,
                    }
                }
            }
        });

        let state_stderr = state.clone();
        let stderr_handle = tokio::spawn(async move {
            let mut reader = stderr_reader.lock().await;
            if let Some(ref mut r) = *reader {
                let mut buf = [0u8; 8192];
                loop {
                    match r.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut s = state_stderr.lock().await;
                            if s.stderr_buffer.len() < max_output {
                                s.stderr_buffer.extend_from_slice(&buf[..n]);
                            }
                            s.last_output_time = chrono::Utc::now();
                        }
                        Err(_) => break,
                    }
                }
            }
        });

        let child_wait = self.child.clone();
        let child_exit = tokio::spawn(async move {
            let mut child_guard = child_wait.lock().await;
            if let Some(ref mut c) = *child_guard {
                c.wait().await
            } else {
                Ok(std::process::ExitStatus::from_raw(0))
            }
        });

        let mut grace_timer: Option<Instant> = None;

        loop {
            if !idle_fired {
                let idle_duration = {
                    let s = state.lock().await;
                    let last_output_time = s.last_output_time;
                    let elapsed = chrono::Utc::now() - last_output_time;
                    elapsed.to_std().unwrap_or(Duration::ZERO)
                };

                if idle_duration >= heartbeat_timeout && heartbeat_timeout.as_secs() > 0 {
                    idle_fired = true;
                    let mut s = state.lock().await;
                    s.timed_out = true;
                    s.timeout_type = Some(TimeoutType::Idle.as_str().to_string());
                    let pid = s.pid;
                    drop(s);

                    tracing::info!(
                        "Idle timeout: no output for {:?} (threshold {:?})",
                        idle_duration,
                        heartbeat_timeout
                    );

                    if let Some(pid) = pid {
                        let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGTERM);
                    }
                    grace_timer = Some(Instant::now());
                }
            }

            if !max_runtime_fired && start.elapsed() >= max_runtime {
                max_runtime_fired = true;
                let mut s = state.lock().await;
                s.timed_out = true;
                s.timeout_type = Some(TimeoutType::MaxRuntime.as_str().to_string());

                tracing::info!(
                    "Max runtime timeout after {:?} (limit {:?})",
                    start.elapsed(),
                    max_runtime
                );

                if let Some(pid) = s.pid {
                    let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGTERM);
                }
                grace_timer = Some(Instant::now());
            }

            if killed_flag.load(Ordering::SeqCst) {
                let mut s = state.lock().await;
                s.killed = true;
            }

            if let Some(gstart) = grace_timer {
                if gstart.elapsed() >= graceful_shutdown {
                    let s = state.lock().await;
                    if let Some(pid) = s.pid {
                        let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
                        tracing::info!("Sent SIGKILL to process group {}", pid);
                    }
                    break;
                }
            }

            if child_exit.is_finished() {
                let exit_status = child_exit.await.ok();
                let mut s = state.lock().await;
                s.completed = true;

                let final_status = if s.timed_out {
                    "timeout"
                } else if s.killed {
                    "killed"
                } else if exit_status.and_then(|r| r.ok()).map(|st| st.success()).unwrap_or(false) {
                    "completed"
                } else {
                    "failed"
                };

                let _ = stdout_handle.await;
                let _ = stderr_handle.await;

                let stdout_str = String::from_utf8_lossy(&s.stdout_buffer).to_string();
                let stderr_str = String::from_utf8_lossy(&s.stderr_buffer).to_string();

                let combined = format!("{}\n{}", stdout_str, stderr_str);
                let (parse_status, completion_payload) = parse_completion(&combined);

                let _native_sid = s.native_session_id.clone().or_else(|| {
                    extract_native_session_id(&combined)
                });

                let elapsed = start.elapsed().as_secs() as i64;

                let result_val = AgentResult {
                    status: final_status.to_string(),
                    summary: completion_payload
                        .as_ref()
                        .map(|p| p.summary.clone())
                        .unwrap_or_default(),
                    stdout: stdout_str,
                    stderr: stderr_str,
                    parse_status: parse_status.as_str().to_string(),
                    completion_signal: if matches!(parse_status, CompletionParseStatus::Parsed) {
                        Some(COMPLETION_MARKER.to_string())
                    } else {
                        None
                    },
                    artifacts: completion_payload
                        .as_ref()
                        .map(|p| p.artifacts.clone())
                        .unwrap_or_default(),
                    changed_files: completion_payload
                        .as_ref()
                        .map(|p| p.changed_files.clone())
                        .unwrap_or_default(),
                    commits: completion_payload
                        .as_ref()
                        .map(|p| p.commits.clone())
                        .unwrap_or_default(),
                    heartbeat_count: s.heartbeat_count,
                    timeout_type: s.timeout_type.clone(),
                    configured_idle_timeout_seconds: heartbeat_timeout.as_secs() as i64,
                    configured_max_runtime_seconds: max_runtime.as_secs() as i64,
                    elapsed_runtime_seconds: elapsed,
                    last_progress_at: Some(s.last_output_time.to_rfc3339()),
                    pid: s.pid.unwrap_or(0),
                };

                let mut r = result.lock().await;
                *r = Some(result_val.clone());
                drop(r);

                let mut d = done.lock().await;
                *d = true;

                return result_val;
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        let _ = stdout_handle.await;
        let _ = stderr_handle.await;

        let mut s = state.lock().await;
        s.completed = true;
        let final_status = if s.timed_out { "timeout" } else { "killed" };

        let stdout_str = String::from_utf8_lossy(&s.stdout_buffer).to_string();
        let stderr_str = String::from_utf8_lossy(&s.stderr_buffer).to_string();
        let combined = format!("{}\n{}", stdout_str, stderr_str);
        let (parse_status, completion_payload) = parse_completion(&combined);

        let elapsed = start.elapsed().as_secs() as i64;

        let result_val = AgentResult {
            status: final_status.to_string(),
            summary: completion_payload
                .as_ref()
                .map(|p| p.summary.clone())
                .unwrap_or_default(),
            stdout: stdout_str,
            stderr: stderr_str,
            parse_status: parse_status.as_str().to_string(),
            completion_signal: if matches!(parse_status, CompletionParseStatus::Parsed) {
                Some(COMPLETION_MARKER.to_string())
            } else {
                None
            },
            artifacts: completion_payload
                .as_ref()
                .map(|p| p.artifacts.clone())
                .unwrap_or_default(),
            changed_files: completion_payload
                .as_ref()
                .map(|p| p.changed_files.clone())
                .unwrap_or_default(),
            commits: completion_payload
                .as_ref()
                .map(|p| p.commits.clone())
                .unwrap_or_default(),
            heartbeat_count: s.heartbeat_count,
            timeout_type: s.timeout_type.clone(),
            configured_idle_timeout_seconds: heartbeat_timeout.as_secs() as i64,
            configured_max_runtime_seconds: max_runtime.as_secs() as i64,
            elapsed_runtime_seconds: elapsed,
            last_progress_at: Some(s.last_output_time.to_rfc3339()),
            pid: s.pid.unwrap_or(0),
        };

        let mut r = result.lock().await;
        *r = Some(result_val.clone());
        drop(r);

        let mut d = done.lock().await;
        *d = true;

        result_val
    }
}

/// Append completion instruction to prompt string.
fn append_completion_instruction(prompt: &str) -> String {
    let mut p = prompt.to_string();
    append_completion_instruction_to(&mut p);
    p
}

fn default_execution_record() -> AgentExecutionRecord {
    let now = chrono::Utc::now().to_rfc3339();
    AgentExecutionRecord {
        id: String::new(),
        project_id: None,
        loop_id: None,
        run_id: None,
        vendor: String::new(),
        status: String::new(),
        pid: None,
        command_json: None,
        cwd: None,
        summary: None,
        parse_status: None,
        completion_signal: None,
        output_json: None,
        error_message: None,
        native_session_id: None,
        native_resume_mode: None,
        native_resume_status: None,
        native_resume_error: None,
        heartbeat_count: 0,
        last_heartbeat_at: None,
        started_at: now.clone(),
        ended_at: None,
        metadata_json: None,
        created_at: now.clone(),
        updated_at: now,
    }
}
