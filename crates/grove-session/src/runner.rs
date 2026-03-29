#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::{
    CheckpointPromptInput, ClaudeBackend, ContextMonitor, ExitPolicy, ParserLineKind,
    PromptMaterializationInput, ProtocolParser, ProtocolWarning, StartSessionRequest,
    TranscriptError, TranscriptWriter, analyze_session_outcome,
    classify_session_outcome_with_policy, materialize_prompt, plan_retry_mutation,
};
use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use grove_types::{
    AgentActivity, BeadId, ClaudeSessionRecord, ContextPressureLevel, EscalationContext,
    EscalationTier, ExecutionContract, FailureClass, MutationStrategy, PromptId, ProtocolEvent,
    ProtocolState, RunId, RuntimeProvider, SessionId, SessionOutcome, SessionStatus,
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
    pub provider: RuntimeProvider,
    pub prompt_id: PromptId,
    pub task_title: String,
    pub task_description: String,
    pub startup_prompt: Option<String>,
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
    pub escalation_tier: EscalationTier,
    pub mutation_strategy: Option<MutationStrategy>,
    pub idle_grace_period: Duration,
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

fn escalation_context_for(
    tier: EscalationTier,
    mutation_strategy: Option<MutationStrategy>,
) -> EscalationContext {
    let instruction = escalation_instruction_for(tier, mutation_strategy);
    EscalationContext {
        tier,
        mutation_strategy,
        tier_number: tier.tier_number(),
        is_terminal: tier.is_terminal(),
        instruction,
    }
}

fn escalation_instruction_for(
    tier: EscalationTier,
    mutation_strategy: Option<MutationStrategy>,
) -> String {
    let strategy_note = match mutation_strategy {
        Some(MutationStrategy::NarrowClaimedPaths) => {
            "You are operating at reduced scope. Prioritize the highest-value remaining step and avoid expanding scope until that step is proven."
        }
        Some(MutationStrategy::DifferentArchiveSnippet) => {
            "Use a different historical context. Draw on different past sessions and archived snippets than the previous attempt."
        }
        Some(MutationStrategy::AlternativeBeadContract) => {
            "Try a fundamentally different approach to the task contract. Re-examine the task goal and pursue an alternative path to the same outcome."
        }
        Some(MutationStrategy::ReduceContextWindow) => {
            "Context pressure is high. Keep your thinking and tool usage concise. Do not expand scope; finish the smallest verifiable step first."
        }
        Some(MutationStrategy::SwitchModel) => {
            "Final attempt before recovery capsule. Exhaust all alternative strategies. Prove the smallest step first before expanding."
        }
        None => match tier {
            EscalationTier::FirstAttempt => "Initial attempt. Proceed normally with full scope.",
            EscalationTier::SecondAttempt => {
                "Second attempt. If stuck, state one hypothesis before editing."
            }
            EscalationTier::ThirdAttempt => {
                "Third attempt. Narrow scope and prioritize the most critical remaining step."
            }
            EscalationTier::FinalAttempt => {
                "Final attempt. Use the most conservative, proven strategy."
            }
            EscalationTier::GiveUp => {
                "This is the last attempt. Create a detailed recovery capsule before ending."
            }
        },
    };
    strategy_note.to_owned()
}

impl SingleTaskSessionRequest {
    #[must_use]
    pub fn escalation_context(&self) -> EscalationContext {
        escalation_context_for(self.escalation_tier, self.mutation_strategy)
    }

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

    #[must_use]
    pub fn with_escalation_context(mut self, context: EscalationContext) -> Self {
        self.escalation_tier = context.tier;
        self.mutation_strategy = context.mutation_strategy;
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
    let escalation_context =
        escalation_context_for(request.escalation_tier, request.mutation_strategy);
    let materialized = materialize_prompt(PromptMaterializationInput {
        prompt_id: request.prompt_id.clone(),
        bead_id: request.bead_id.clone(),
        run_id: request.run_id.clone(),
        created_at: started_at,
        contract: request.contract,
        task_title: request.task_title.clone(),
        task_description: request.task_description.clone(),
        startup_prompt: request.startup_prompt.clone(),
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
        escalation_context: Some(escalation_context),
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
        provider: request.provider,
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
    let mut last_stream_activity_at = Instant::now();

    let result = (|| {
        let mut running = backend.start(StartSessionRequest {
            provider: request.provider,
            model: request.model.clone(),
            prompt: materialized.rendered_prompt.clone(),
            working_dir: request.working_dir.clone(),
            timeout: request.timeout,
            env: request.env.clone(),
        })?;

        let (sender, receiver) = mpsc::channel();
        let stdout_handle =
            spawn_stream_forwarder(running.stdout, sender.clone(), StreamSource::Stdout);
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
                    if last_activity != AgentActivity::Idle
                        && last_stream_activity_at.elapsed() >= request.idle_grace_period
                    {
                        hooks
                            .on_activity_changed(AgentActivity::Idle, Some("stream_timeout"), ts)
                            .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;
                        last_activity = AgentActivity::Idle;
                    }
                    last_stream_activity_at = Instant::now();
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
                    if last_activity != AgentActivity::Idle
                        && last_stream_activity_at.elapsed() >= request.idle_grace_period
                    {
                        hooks
                            .on_activity_changed(AgentActivity::Idle, Some("stream_timeout"), ts)
                            .map_err(SingleTaskSessionRunnerError::LifecycleHook)?;
                        last_activity = AgentActivity::Idle;
                    }
                    last_stream_activity_at = Instant::now();
                    transcript.append_stderr_line(line.clone(), ts)?;
                    if let ParserLineKind::PlainStderr(text) = parser.parse_stderr_line(&line) {
                        let next_activity =
                            if text.to_ascii_lowercase().contains("permission denied") {
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
                    if last_activity != AgentActivity::Idle
                        && last_stream_activity_at.elapsed() >= request.idle_grace_period
                    {
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

                if let Some(deadline) = grace_deadline
                    && !kill_sent
                    && Instant::now() >= deadline
                {
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
            let mode =
                crate::verify::VerificationMode::infer(request.contract, &request.working_dir);
            if let Err(verify_err) =
                crate::verify::run_verification(mode, &request.working_dir, &materialized.manifest)
            {
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
            provider: request.provider,
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
            let message_source = source;
            if sender
                .send(StreamMessage::Line(message_source, line))
                .is_err()
            {
                return;
            }
        }
        let closed_source = source;
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
        provider: request.provider,
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
        "Do not use EnterPlanMode, ExitPlanMode, or AskUserQuestion during Grove task execution.",
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

#[cfg(all(test, unix))]
mod tests;
