use crate::{
    CheckpointPromptInput, ClaudeBackend, ContextMonitor, ExitPolicy, ParserLineKind,
    PromptMaterializationInput, ProtocolParser, ProtocolWarning, StartSessionRequest,
    TranscriptError, TranscriptWriter, analyze_session_outcome,
    classify_session_outcome_with_policy, materialize_prompt, plan_retry_mutation,
};
use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use grove_types::{
    AgentActivity, BeadId, ClaudeSessionRecord, ContextPressureLevel, ExecutionContract, FailureClass,
    PromptId, ProtocolEvent, ProtocolState, RunId, SessionId, SessionOutcome, SessionStatus,
    SessionTerminalClass, StopReason,
};
use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Debug, Clone, Default)]
pub struct SessionShutdownConfig {
    pub signal: Option<Arc<AtomicBool>>,
    pub grace_period: Option<Duration>,
}

impl SessionShutdownConfig {
    #[must_use]
    pub fn is_requested(&self) -> bool {
        self.signal
            .as_ref()
            .is_some_and(|signal| signal.load(Ordering::SeqCst))
    }
}

#[derive(Debug, Clone)]
pub struct SingleTaskSessionRequest {
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub prompt_id: PromptId,
    pub task_title: String,
    pub task_description: String,
    pub contract: ExecutionContract,
    pub model: String,
    pub working_dir: Utf8PathBuf,
    pub transcript_path: Utf8PathBuf,
    pub prompt_manifest_path: Utf8PathBuf,
    pub timeout: std::time::Duration,
    pub exit_policy: ExitPolicy,
    pub context_monitor: ContextMonitor,
    pub reservation_hints: Vec<String>,
    pub parent_handoffs: Vec<String>,
    pub checkpoint: Option<CheckpointPromptInput>,
    pub previous_failure_class: Option<FailureClass>,
    pub previous_outcome: Option<SessionOutcome>,
    pub rescue_card: Option<String>,
    pub retry_delta_summary: Option<String>,
    pub retrieval_query: Option<String>,
    pub token_budget: Option<u32>,
    pub ordinal_in_run: i32,
    pub archive_bundle: Option<grove_types::archive::RetrievalBundle>,
    pub playbook_rules: Vec<grove_types::playbook::PlaybookBulletRecord>,
    pub env: Vec<(String, String)>,
    pub shutdown: SessionShutdownConfig,
}

#[derive(Debug, Clone)]
pub struct SingleTaskSessionResult {
    pub outcome: SessionOutcome,
    pub protocol_state: ProtocolState,
    pub protocol_warnings: Vec<ProtocolWarning>,
}

pub trait SessionLifecycleHooks {
    fn on_session_started(&mut self, _session: &ClaudeSessionRecord) -> anyhow::Result<()> {
        Ok(())
    }

    fn on_activity_changed(
        &mut self,
        _activity: AgentActivity,
        _detail: Option<&str>,
        _at: chrono::DateTime<Utc>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn on_shutdown_requested(&mut self, _grace_period: Option<Duration>) -> anyhow::Result<()> {
        Ok(())
    }

    fn on_shutdown_forced(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn on_session_finished(&mut self, _result: &SingleTaskSessionResult) -> anyhow::Result<()> {
        Ok(())
    }
}

impl SessionLifecycleHooks for () {}

impl SingleTaskSessionRequest {
    #[must_use]
    pub fn with_retry_context(
        mut self,
        failure_class: FailureClass,
        previous_outcome: Option<SessionOutcome>,
    ) -> Self {
        let plan = plan_retry_mutation(failure_class, previous_outcome.as_ref());
        self.contract = plan.next_contract;
        self.previous_failure_class = Some(failure_class);
        self.previous_outcome = previous_outcome;
        self.retry_delta_summary = Some(plan.retry_delta_summary);
        self.rescue_card = Some(plan.rescue_card);
        self
    }
}

#[derive(Debug, Error)]
pub enum SingleTaskSessionRunnerError {
    #[error(transparent)]
    BackendStart(#[from] anyhow::Error),
    #[error(transparent)]
    Transcript(#[from] TranscriptError),
    #[error("failed to persist prompt manifest {path}: {source}")]
    PersistPromptManifest {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to encode prompt manifest {path}: {source}")]
    EncodePromptManifest {
        path: String,
        source: serde_json::Error,
    },
    #[error("failed to read stdout from Claude session: {0}")]
    ReadStdout(std::io::Error),
    #[error("failed to read stderr from Claude session: {0}")]
    ReadStderr(std::io::Error),
    #[error("failed to wait for Claude session: {0}")]
    Wait(std::io::Error),
    #[error("failed to kill Claude session: {0}")]
    Kill(std::io::Error),
    #[error("session lifecycle hook failed: {0}")]
    LifecycleHook(anyhow::Error),
}

pub fn execute_single_task_session<B: ClaudeBackend>(
    backend: &B,
    request: SingleTaskSessionRequest,
) -> Result<SingleTaskSessionResult, SingleTaskSessionRunnerError> {
    execute_single_task_session_with_hooks(backend, request, &mut ())
}

pub fn execute_single_task_session_with_hooks<B: ClaudeBackend, H: SessionLifecycleHooks>(
    backend: &B,
    request: SingleTaskSessionRequest,
    hooks: &mut H,
) -> Result<SingleTaskSessionResult, SingleTaskSessionRunnerError> {
    let started_at = Utc::now();
    let protocol_block = default_protocol_block();
    let materialized = materialize_prompt(PromptMaterializationInput {
        prompt_id: request.prompt_id.clone(),
        bead_id: request.bead_id.clone(),
        run_id: request.run_id.clone(),
        created_at: started_at,
        contract: request.contract,
        task_title: request.task_title.clone(),
        task_description: request.task_description.clone(),
        reservation_hints: request.reservation_hints.clone(),
        parent_handoffs: request.parent_handoffs.clone(),
        checkpoint: request.checkpoint.clone(),
        protocol_block,
        rescue_card: request.rescue_card.clone(),
        token_budget: request.token_budget,
        retry_delta_summary: request.retry_delta_summary.clone(),
        retrieval_query: request.retrieval_query.clone(),
        archive_bundle: request.archive_bundle.clone(),
        playbook_rules: request.playbook_rules.clone(),
    });

    let transcript_abs = resolve_under_working_dir(&request.working_dir, &request.transcript_path);
    let prompt_manifest_abs =
        resolve_under_working_dir(&request.working_dir, &request.prompt_manifest_path);

    let mut manifest = materialized.manifest.clone();
    manifest.session_id = Some(request.session_id.clone());
    persist_prompt_manifest(prompt_manifest_abs.as_std_path(), &manifest)?;

    let mut transcript = TranscriptWriter::open(transcript_abs.as_std_path())?;
    transcript.append_session_started(request.session_id.clone(), started_at)?;

    let started_session = ClaudeSessionRecord {
        id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        external_session_id: None,
        ordinal_in_run: request.ordinal_in_run,
        status: SessionStatus::Running,
        started_at,
        ended_at: None,
        prompt_id: Some(request.prompt_id.clone()),
        prompt_manifest_path: Some(request.prompt_manifest_path.to_string()),
        prompt_bytes: materialized.prompt_bytes as i32,
        estimated_input_tokens: materialized.estimated_input_tokens as i32,
        estimated_output_tokens: 0,
        exit_code: None,
        stop_reason: None,
        transcript_path: request.transcript_path.to_string(),
    };
    hooks
        .on_session_started(&started_session)
        .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;

    let mut parser = ProtocolParser::default();
    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();
    let mut last_activity = AgentActivity::Active;

    let result = (|| {
        let mut running = backend.start(StartSessionRequest {
            model: request.model.clone(),
            prompt: materialized.rendered_prompt.clone(),
            working_dir: request.working_dir.clone(),
            timeout: request.timeout,
            env: request.env.clone(),
        })?;

        let (sender, receiver) = mpsc::channel();
        let stdout_handle = spawn_stream_forwarder(running.stdout, sender.clone(), StreamSource::Stdout);
        let stderr_handle = spawn_stream_forwarder(running.stderr, sender, StreamSource::Stderr);

        let mut stdout_closed = false;
        let mut stderr_closed = false;
        let mut exit_status = None;
        let mut forced_shutdown = false;
        let mut kill_sent = false;
        let mut grace_deadline = None;

        while exit_status.is_none() || !stdout_closed || !stderr_closed {
            match receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(StreamMessage::Line(StreamSource::Stdout, line)) => {
                    let line = line.map_err(SingleTaskSessionRunnerError::ReadStdout)?;
                    let ts = Utc::now();
                    transcript.append_stdout_line(line.clone(), ts)?;
                    match parser.parse_stdout_line(&line) {
                        ParserLineKind::Protocol(event) => {
                            transcript.append_protocol_event(event.clone(), ts)?;
                            let next_activity = match event {
                                ProtocolEvent::Checkpoint { .. } => AgentActivity::Ready,
                                ProtocolEvent::Exit { value: true } => AgentActivity::Ready,
                                _ => AgentActivity::Active,
                            };
                            if next_activity != last_activity {
                                hooks
                                    .on_activity_changed(next_activity, Some("protocol_event"), ts)
                                    .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;
                                last_activity = next_activity;
                            }
                        }
                        ParserLineKind::PlainStdout(text) => {
                            stdout_lines.push(text);
                            if last_activity != AgentActivity::Active {
                                hooks
                                    .on_activity_changed(AgentActivity::Active, Some("stdout"), ts)
                                    .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;
                                last_activity = AgentActivity::Active;
                            }
                        }
                        ParserLineKind::PlainStderr(_) => {}
                    }
                }
                Ok(StreamMessage::Line(StreamSource::Stderr, line)) => {
                    let line = line.map_err(SingleTaskSessionRunnerError::ReadStderr)?;
                    let ts = Utc::now();
                    transcript.append_stderr_line(line.clone(), ts)?;
                    if let ParserLineKind::PlainStderr(text) = parser.parse_stderr_line(&line) {
                        let next_activity = if text.to_ascii_lowercase().contains("permission denied") {
                            AgentActivity::Blocked
                        } else {
                            AgentActivity::Active
                        };
                        stderr_lines.push(text);
                        if next_activity != last_activity {
                            hooks
                                .on_activity_changed(next_activity, Some("stderr"), ts)
                                .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;
                            last_activity = next_activity;
                        }
                    }
                }
                Ok(StreamMessage::Closed(StreamSource::Stdout)) => stdout_closed = true,
                Ok(StreamMessage::Closed(StreamSource::Stderr)) => stderr_closed = true,
                Err(RecvTimeoutError::Timeout) => {
                    let ts = Utc::now();
                    if last_activity != AgentActivity::Idle {
                        hooks
                            .on_activity_changed(AgentActivity::Idle, Some("stream_timeout"), ts)
                            .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;
                        last_activity = AgentActivity::Idle;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    stdout_closed = true;
                    stderr_closed = true;
                }
            }

            if exit_status.is_none() {
                if request.shutdown.is_requested() && grace_deadline.is_none() {
                    hooks
                        .on_shutdown_requested(request.shutdown.grace_period)
                        .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;
                    grace_deadline = Some(
                        Instant::now() + request.shutdown.grace_period.unwrap_or(Duration::ZERO),
                    );
                }

                if let Some(deadline) = grace_deadline {
                    if !kill_sent && Instant::now() >= deadline {
                        running
                            .child
                            .kill()
                            .map_err(SingleTaskSessionRunnerError::Kill)?;
                        hooks
                            .on_shutdown_forced()
                            .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;
                        kill_sent = true;
                        forced_shutdown = true;
                    }
                }

                if let Some(status) = running
                    .child
                    .try_wait()
                    .map_err(SingleTaskSessionRunnerError::Wait)?
                {
                    exit_status = Some(status);
                }
            }
        }

        let _ = stdout_handle.join();
        let _ = stderr_handle.join();

        let status = match exit_status {
            Some(status) => status,
            None => running
                .child
                .wait()
                .map_err(SingleTaskSessionRunnerError::Wait)?,
        };
        let ended_at = Utc::now();
        transcript.append_session_ended(status.code(), ended_at)?;

        let protocol_warnings = parser.warnings().to_vec();
        let protocol_state = parser.into_state();
        let analysis = analyze_session_outcome(crate::SessionAnalysisContext {
            protocol_state: &protocol_state,
            protocol_warnings: &protocol_warnings,
            stdout_lines: &stdout_lines,
            stderr_lines: &stderr_lines,
            estimated_prompt_tokens: materialized.estimated_input_tokens,
            estimated_output_tokens: estimate_output_tokens(&stdout_lines, &stderr_lines),
        });
        let context_pressure = request.context_monitor.estimate(&analysis);
        let context_pressure_level = request.context_monitor.classify(&analysis);
        let mut terminal_class = classify_session_outcome_with_policy(
            &request.exit_policy,
            &analysis,
            status.code(),
            false,
        );

        if forced_shutdown {
            terminal_class = SessionTerminalClass::UnknownFailure;
        }

        // Run Verification before closing out Success.
        if terminal_class == SessionTerminalClass::Success {
            let mode = crate::verify::VerificationMode::infer(
                request.contract,
                &request.working_dir,
            );
            if let Err(verify_err) = crate::verify::run_verification(mode, &request.working_dir, &materialized.manifest) {
                // If verification failed, log it to stderr so it's captured in the analysis tail
                // and fail the terminal class.
                let msg = format!("grove verification failed:\n{}", verify_err);
                eprintln!("{}", msg);
                stderr_lines.push(msg);
                terminal_class = SessionTerminalClass::VerifyFailed;
            }
        }

        let session = ClaudeSessionRecord {
            id: request.session_id.clone(),
            run_id: request.run_id.clone(),
            external_session_id: None,
            ordinal_in_run: request.ordinal_in_run,
            status: if forced_shutdown {
                SessionStatus::UnknownFailure
            } else {
                status_from_terminal_class(terminal_class)
            },
            started_at,
            ended_at: Some(ended_at),
            prompt_id: Some(request.prompt_id.clone()),
            prompt_manifest_path: Some(request.prompt_manifest_path.to_string()),
            prompt_bytes: materialized.prompt_bytes as i32,
            estimated_input_tokens: materialized.estimated_input_tokens as i32,
            estimated_output_tokens: analysis.estimated_output_tokens as i32,
            exit_code: status.code(),
            stop_reason: Some(if forced_shutdown {
                StopReason::Kill
            } else {
                stop_reason_from_terminal_class(terminal_class)
            }),
            transcript_path: request.transcript_path.to_string(),
        };

        Ok(SingleTaskSessionResult {
            outcome: SessionOutcome {
                session,
                protocol_events: protocol_state.events.clone(),
                analysis,
                terminal_class,
                context_pressure_pct: Some(context_pressure.usage_pct),
                context_pressure_level,
                stdout_tail: tail(&stdout_lines),
                stderr_tail: tail(&stderr_lines),
            },
            protocol_state,
            protocol_warnings,
        })
    })();

    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let result = failure_result_from_error(
                &request,
                &materialized,
                started_at,
                &stdout_lines,
                &stderr_lines,
                &error,
            );
            hooks
                .on_session_finished(&result)
                .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;
            return Err(error);
        }
    };

    hooks
        .on_session_finished(&result)
        .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;

    Ok(result)
}

#[derive(Clone, Copy)]
enum StreamSource {
    Stdout,
    Stderr,
}

enum StreamMessage {
    Line(StreamSource, Result<String, std::io::Error>),
    Closed(StreamSource),
}

fn spawn_stream_forwarder(
    mut lines: impl Iterator<Item = Result<String, std::io::Error>> + Send + 'static,
    sender: mpsc::Sender<StreamMessage>,
    source: StreamSource,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in lines.by_ref() {
            let message_source = match source {
                StreamSource::Stdout => StreamSource::Stdout,
                StreamSource::Stderr => StreamSource::Stderr,
            };
            if sender.send(StreamMessage::Line(message_source, line)).is_err() {
                return;
            }
        }
        let closed_source = match source {
            StreamSource::Stdout => StreamSource::Stdout,
            StreamSource::Stderr => StreamSource::Stderr,
        };
        let _ = sender.send(StreamMessage::Closed(closed_source));
    })
}

fn resolve_under_working_dir(base: &Utf8Path, path: &Utf8Path) -> Utf8PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}

fn persist_prompt_manifest(
    path: &Path,
    manifest: &grove_types::PromptManifest,
) -> Result<(), SingleTaskSessionRunnerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| {
            SingleTaskSessionRunnerError::PersistPromptManifest {
                path: parent.display().to_string(),
                source,
            }
        })?;
    }
    let encoded = serde_json::to_vec_pretty(manifest).map_err(|source| {
        SingleTaskSessionRunnerError::EncodePromptManifest {
            path: path.display().to_string(),
            source,
        }
    })?;
    fs::write(path, encoded).map_err(
        |source| SingleTaskSessionRunnerError::PersistPromptManifest {
            path: path.display().to_string(),
            source,
        },
    )
}

fn failure_result_from_error(
    request: &SingleTaskSessionRequest,
    materialized: &crate::PromptMaterialization,
    started_at: chrono::DateTime<Utc>,
    stdout_lines: &[String],
    stderr_lines: &[String],
    error: &SingleTaskSessionRunnerError,
) -> SingleTaskSessionResult {
    let ended_at = Utc::now();
    let terminal_class = terminal_class_from_error(error);
    let analysis = analyze_session_outcome(crate::SessionAnalysisContext {
        protocol_state: &ProtocolState::default(),
        protocol_warnings: &[],
        stdout_lines,
        stderr_lines,
        estimated_prompt_tokens: materialized.estimated_input_tokens,
        estimated_output_tokens: estimate_output_tokens(stdout_lines, stderr_lines),
    });
    let session = ClaudeSessionRecord {
        id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        external_session_id: None,
        ordinal_in_run: request.ordinal_in_run,
        status: status_from_terminal_class(terminal_class),
        started_at,
        ended_at: Some(ended_at),
        prompt_id: Some(request.prompt_id.clone()),
        prompt_manifest_path: Some(request.prompt_manifest_path.to_string()),
        prompt_bytes: materialized.prompt_bytes as i32,
        estimated_input_tokens: materialized.estimated_input_tokens as i32,
        estimated_output_tokens: analysis.estimated_output_tokens as i32,
        exit_code: None,
        stop_reason: Some(stop_reason_from_terminal_class(terminal_class)),
        transcript_path: request.transcript_path.to_string(),
    };

    SingleTaskSessionResult {
        outcome: SessionOutcome {
            session,
            protocol_events: Vec::new(),
            analysis,
            terminal_class,
            context_pressure_pct: None,
            context_pressure_level: ContextPressureLevel::Ok,
            stdout_tail: tail(stdout_lines),
            stderr_tail: tail(stderr_lines),
        },
        protocol_state: ProtocolState::default(),
        protocol_warnings: Vec::new(),
    }
}

fn terminal_class_from_error(error: &SingleTaskSessionRunnerError) -> SessionTerminalClass {
    match error {
        SingleTaskSessionRunnerError::ReadStdout(_)
        | SingleTaskSessionRunnerError::ReadStderr(_)
        | SingleTaskSessionRunnerError::Wait(_) => SessionTerminalClass::Crash,
        SingleTaskSessionRunnerError::BackendStart(_) | SingleTaskSessionRunnerError::Kill(_) => {
            SessionTerminalClass::UnknownFailure
        }
        SingleTaskSessionRunnerError::Transcript(_)
        | SingleTaskSessionRunnerError::PersistPromptManifest { .. }
        | SingleTaskSessionRunnerError::EncodePromptManifest { .. }
        | SingleTaskSessionRunnerError::LifecycleHook(_) => SessionTerminalClass::UnknownFailure,
    }
}

fn default_protocol_block() -> String {
    [
        "[GROVE PROTOCOL]",
        "Emit GROVE_RESULT, GROVE_ARTIFACTS, GROVE_LESSONS, GROVE_DECISIONS, GROVE_WARNINGS, and GROVE_EXIT markers on stdout when appropriate.",
        "Emit GROVE_CHECKPOINT with structured JSON before rotating on context pressure.",
        "Use GROVE_EXIT: false while work is still in progress.",
    ]
    .join("\n")
}

fn estimate_output_tokens(stdout_lines: &[String], stderr_lines: &[String]) -> u32 {
    let chars = stdout_lines
        .iter()
        .chain(stderr_lines.iter())
        .map(|line| line.chars().count())
        .sum::<usize>();
    chars.div_ceil(4) as u32
}

fn tail(lines: &[String]) -> Vec<String> {
    const MAX_TAIL: usize = 20;
    let start = lines.len().saturating_sub(MAX_TAIL);
    lines[start..].to_vec()
}

fn status_from_terminal_class(terminal_class: SessionTerminalClass) -> SessionStatus {
    match terminal_class {
        SessionTerminalClass::Success => SessionStatus::Completed,
        SessionTerminalClass::Checkpoint => SessionStatus::Checkpointed,
        SessionTerminalClass::Timeout => SessionStatus::TimedOut,
        SessionTerminalClass::RateLimit => SessionStatus::RateLimited,
        SessionTerminalClass::PermissionDenied => SessionStatus::PermissionDenied,
        SessionTerminalClass::Crash => SessionStatus::Crashed,
        SessionTerminalClass::VerifyFailed => SessionStatus::UnknownFailure,
        SessionTerminalClass::UnknownFailure => SessionStatus::UnknownFailure,
    }
}

fn stop_reason_from_terminal_class(terminal_class: SessionTerminalClass) -> StopReason {
    match terminal_class {
        SessionTerminalClass::Success => StopReason::Exit,
        SessionTerminalClass::Checkpoint => StopReason::Checkpoint,
        SessionTerminalClass::Timeout => StopReason::Timeout,
        SessionTerminalClass::RateLimit => StopReason::RateLimit,
        SessionTerminalClass::PermissionDenied => StopReason::PermissionDenied,
        SessionTerminalClass::Crash => StopReason::Crash,
        SessionTerminalClass::VerifyFailed => StopReason::Unknown, // we don't have a specific StopReason for verify failure yet
        SessionTerminalClass::UnknownFailure => StopReason::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CliClaudeBackend, replay_transcript};
    use grove_types::{
        FailureClass, IterationAnalysis, ProgressSignal, PromptManifest, PromptSegmentKind,
    };
    use std::{error::Error, fs, io, time::Duration};
    use tempfile::tempdir;

    type TestResult = Result<(), Box<dyn Error>>;

    #[cfg(unix)]
    fn write_fake_claude_script(path: &std::path::Path) -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let script = r#"#!/bin/sh
printf '%b' "$STDOUT_SCRIPT"
printf '%b' "$STDERR_SCRIPT" >&2
exit "${EXIT_CODE:-0}"
"#;
        fs::write(path, script)?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
        Ok(())
    }

    #[cfg(unix)]
    fn sample_request(workspace_dir: Utf8PathBuf) -> SingleTaskSessionRequest {
        SingleTaskSessionRequest {
            bead_id: BeadId::new("grove-1j9.6.8"),
            run_id: RunId::new("run-1"),
            session_id: SessionId::new("ses-1"),
            prompt_id: PromptId::new("prompt-1"),
            task_title: "Implement single task runner".to_owned(),
            task_description: "Wire the session subsystem end to end.".to_owned(),
            contract: ExecutionContract::SingleTask,
            model: "sonnet".to_owned(),
            working_dir: workspace_dir,
            transcript_path: Utf8PathBuf::from(".grove/transcripts/grove-1j9.6.8/ses-1.jsonl"),
            prompt_manifest_path: Utf8PathBuf::from(".grove/prompts/prompt-1.json"),
            timeout: Duration::from_secs(60),
            exit_policy: ExitPolicy::default(),
            context_monitor: ContextMonitor::new(0.7, 0.82, 0.9, 16_000),
            reservation_hints: vec!["crates/grove-session/src/**".to_owned()],
            parent_handoffs: vec!["Phase 2 components are ready to be joined.".to_owned()],
            checkpoint: None,
            previous_failure_class: None,
            previous_outcome: None,
            rescue_card: None,
            retry_delta_summary: None,
            retrieval_query: None,
            token_budget: Some(2_000),
            ordinal_in_run: 1,
            archive_bundle: None,
            playbook_rules: vec![],
            env: Vec::new(),
            shutdown: SessionShutdownConfig::default(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn execute_single_task_session_returns_structured_outcome_and_artifacts() -> TestResult {
        let dir = tempdir()?;
        let workspace_dir = dir.path().join("workspace");
        fs::create_dir_all(&workspace_dir)?;
        let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
            .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;

        let script_path = dir.path().join("fake-claude");
        write_fake_claude_script(&script_path)?;
        let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

        let mut request = sample_request(workspace_dir.clone());
        request.env = vec![
            (
                "STDOUT_SCRIPT".to_owned(),
                concat!(
                    "working through the task\n",
                    "GROVE_RESULT: session runner wired\n",
                    "GROVE_ARTIFACTS: [\"crates/grove-session/src/runner.rs\"]\n",
                    "GROVE_EXIT: true\n",
                    "all tasks complete\n",
                    "implementation complete\n"
                )
                .to_owned(),
            ),
            ("STDERR_SCRIPT".to_owned(), "minor stderr note\n".to_owned()),
            ("EXIT_CODE".to_owned(), "0".to_owned()),
        ];

        let result = execute_single_task_session(&backend, request)?;

        assert_eq!(result.outcome.session.id.as_str(), "ses-1");
        assert_eq!(result.outcome.session.status, SessionStatus::Completed);
        assert_eq!(result.outcome.terminal_class, SessionTerminalClass::Success);
        assert_eq!(
            result.protocol_state.result_summary.as_deref(),
            Some("session runner wired")
        );
        assert_eq!(
            result.protocol_state.artifacts,
            vec!["crates/grove-session/src/runner.rs".to_owned()]
        );
        assert_eq!(
            result.outcome.context_pressure_level,
            grove_types::ContextPressureLevel::Ok
        );

        let prompt_path = workspace_dir.join(".grove/prompts/prompt-1.json");
        let transcript_path = workspace_dir.join(".grove/transcripts/grove-1j9.6.8/ses-1.jsonl");
        assert!(prompt_path.exists());
        assert!(transcript_path.exists());

        let replay = replay_transcript(transcript_path.as_std_path())?;
        assert!(replay.events.len() >= 5);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn execute_single_task_session_persists_retry_rescue_manifest_details() -> TestResult {
        let dir = tempdir()?;
        let workspace_dir = dir.path().join("workspace");
        fs::create_dir_all(&workspace_dir)?;
        let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
            .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;

        let script_path = dir.path().join("fake-claude");
        write_fake_claude_script(&script_path)?;
        let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

        let mut request = sample_request(workspace_dir.clone());
        request.contract = ExecutionContract::RetryRescue;
        request.rescue_card = Some("Avoid replaying the repeated parse failure.".to_owned());
        request.retry_delta_summary =
            Some("Changed retry framing: use a different verification path.".to_owned());
        request.env = vec![
            (
                "STDOUT_SCRIPT".to_owned(),
                concat!(
                    "retrying with changed approach\n",
                    "GROVE_RESULT: retry rescue planned\n",
                    "GROVE_EXIT: true\n",
                    "all tasks complete\n",
                    "implementation complete\n"
                )
                .to_owned(),
            ),
            ("STDERR_SCRIPT".to_owned(), String::new()),
            ("EXIT_CODE".to_owned(), "0".to_owned()),
        ];

        let result = execute_single_task_session(&backend, request)?;

        assert_eq!(
            result
                .outcome
                .session
                .prompt_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("prompt-1")
        );
        assert_eq!(
            result.outcome.session.prompt_manifest_path.as_deref(),
            Some(".grove/prompts/prompt-1.json")
        );

        let prompt_path = workspace_dir.join(".grove/prompts/prompt-1.json");
        let manifest: PromptManifest =
            serde_json::from_str(&fs::read_to_string(prompt_path.as_std_path())?)?;
        assert_eq!(
            manifest.session_id.as_ref().map(|id| id.as_str()),
            Some("ses-1")
        );
        assert_eq!(manifest.contract, ExecutionContract::RetryRescue);
        assert_eq!(
            manifest.retry_delta_summary.as_deref(),
            Some("Changed retry framing: use a different verification path.")
        );
        let rescue_card = manifest
            .sections
            .iter()
            .find(|section| section.kind == PromptSegmentKind::RescueCard)
            .ok_or("missing rescue-card section")?;
        assert!(rescue_card.included);
        assert!(
            rescue_card
                .preview
                .contains("Avoid replaying the repeated parse failure.")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn execute_single_task_session_classifies_rate_limit_and_preserves_tails() -> TestResult {
        let dir = tempdir()?;
        let workspace_dir = dir.path().join("workspace");
        fs::create_dir_all(&workspace_dir)?;
        let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
            .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;

        let script_path = dir.path().join("fake-claude");
        write_fake_claude_script(&script_path)?;
        let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

        let mut request = sample_request(workspace_dir);
        request.env = vec![
            (
                "STDOUT_SCRIPT".to_owned(),
                "rate limit exceeded by upstream\n".to_owned(),
            ),
            (
                "STDERR_SCRIPT".to_owned(),
                "ratelimit retry window still active\n".to_owned(),
            ),
            ("EXIT_CODE".to_owned(), "1".to_owned()),
        ];

        let result = execute_single_task_session(&backend, request)?;

        assert_eq!(
            result.outcome.terminal_class,
            SessionTerminalClass::RateLimit
        );
        assert_eq!(result.outcome.session.status, SessionStatus::RateLimited);
        assert_eq!(
            result.outcome.stdout_tail,
            vec!["rate limit exceeded by upstream".to_owned()]
        );
        assert_eq!(
            result.outcome.stderr_tail,
            vec!["ratelimit retry window still active".to_owned()]
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn with_retry_context_derives_retry_rescue_metadata_from_previous_outcome() -> TestResult {
        let dir = tempdir()?;
        let workspace_dir = dir.path().join("workspace");
        fs::create_dir_all(&workspace_dir)?;
        let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
            .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;

        let script_path = dir.path().join("fake-claude");
        write_fake_claude_script(&script_path)?;
        let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

        let previous_outcome = SessionOutcome {
            session: ClaudeSessionRecord {
                id: SessionId::new("ses-prev"),
                run_id: RunId::new("run-prev"),
                external_session_id: None,
                ordinal_in_run: 1,
                status: SessionStatus::UnknownFailure,
                started_at: Utc::now(),
                ended_at: Some(Utc::now()),
                prompt_id: None,
                prompt_manifest_path: None,
                prompt_bytes: 0,
                estimated_input_tokens: 0,
                estimated_output_tokens: 0,
                exit_code: Some(1),
                stop_reason: Some(StopReason::Unknown),
                transcript_path: ".grove/transcripts/grove-1j9.6.7/ses-prev.jsonl".to_owned(),
            },
            protocol_events: vec![],
            analysis: IterationAnalysis {
                probable_progress: ProgressSignal::Weak,
                repeated_error_fingerprint: Some(
                    "error: protocol marker was malformed and failed to parse".to_owned(),
                ),
                has_explicit_exit_false: true,
                ..IterationAnalysis::default()
            },
            terminal_class: SessionTerminalClass::UnknownFailure,
            context_pressure_pct: None,
            context_pressure_level: grove_types::ContextPressureLevel::Ok,
            stdout_tail: Vec::new(),
            stderr_tail: Vec::new(),
        };

        let mut request = sample_request(workspace_dir.clone())
            .with_retry_context(FailureClass::RepeatedError, Some(previous_outcome));
        request.env = vec![
            (
                "STDOUT_SCRIPT".to_owned(),
                concat!(
                    "retry attempt with changed approach\n",
                    "GROVE_RESULT: retry mutation applied\n",
                    "GROVE_EXIT: true\n",
                    "implementation complete\n"
                )
                .to_owned(),
            ),
            ("STDERR_SCRIPT".to_owned(), String::new()),
            ("EXIT_CODE".to_owned(), "0".to_owned()),
        ];

        assert_eq!(
            request.previous_failure_class,
            Some(FailureClass::RepeatedError)
        );
        assert_eq!(request.contract, ExecutionContract::RetryRescue);
        assert!(
            request
                .retry_delta_summary
                .as_deref()
                .is_some_and(|summary| { summary.contains("repeated error path") })
        );
        assert!(
            request
                .rescue_card
                .as_deref()
                .is_some_and(|card| card.contains("Previous repeated error to avoid"))
        );

        let result = execute_single_task_session(&backend, request)?;

        let prompt_path = workspace_dir.join(".grove/prompts/prompt-1.json");
        let manifest: PromptManifest =
            serde_json::from_str(&fs::read_to_string(prompt_path.as_std_path())?)?;
        assert_eq!(manifest.contract, ExecutionContract::RetryRescue);
        assert!(
            manifest
                .retry_delta_summary
                .as_deref()
                .is_some_and(|summary| { summary.contains("repeated error path") })
        );
        let rescue_card = manifest
            .sections
            .iter()
            .find(|section| section.kind == PromptSegmentKind::RescueCard)
            .ok_or("missing rescue-card section")?;
        assert!(
            rescue_card
                .preview
                .contains("Do not repeat the same failing path")
        );
        assert_eq!(
            result.protocol_state.result_summary.as_deref(),
            Some("retry mutation applied")
        );
        Ok(())
    }
}
