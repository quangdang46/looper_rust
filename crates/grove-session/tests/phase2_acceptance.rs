#![cfg(unix)]

use std::{
    error::Error,
    fs, io,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};

use camino::Utf8PathBuf;
use chrono::Utc;
use grove_session::{
    CliClaudeBackend, ContextMonitor, ExitPolicy, SessionLifecycleHooks, SessionShutdownConfig,
    SingleTaskSessionRequest, execute_single_task_session, execute_single_task_session_with_hooks,
};
use grove_types::{
    BeadId, ClaudeSessionRecord, ContextPressureLevel, ExecutionContract, FailureClass,
    IterationAnalysis, ProgressSignal, PromptId, PromptManifest, PromptSegmentKind, RunId,
    SessionId, SessionOutcome, SessionStatus, SessionTerminalClass, StopReason,
};
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn Error>>;

fn write_fake_claude_script(path: &std::path::Path) -> TestResult {
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

fn sample_request(workspace_dir: Utf8PathBuf) -> SingleTaskSessionRequest {
    SingleTaskSessionRequest {
        bead_id: BeadId::new("grove-1j9.6.9"),
        run_id: RunId::new("run-1"),
        session_id: SessionId::new("ses-1"),
        prompt_id: PromptId::new("prompt-1"),
        task_title: "Phase 2 acceptance coverage".to_owned(),
        task_description: "Prove one-task execution semantics before Phase 3 depends on them."
            .to_owned(),
        contract: ExecutionContract::SingleTask,
        model: "sonnet".to_owned(),
        working_dir: workspace_dir,
        transcript_path: Utf8PathBuf::from(".grove/transcripts/grove-1j9.6.9/ses-1.jsonl"),
        prompt_manifest_path: Utf8PathBuf::from(".grove/prompts/prompt-1.json"),
        timeout: Duration::from_secs(60),
        exit_policy: ExitPolicy::default(),
        context_monitor: ContextMonitor::new(0.7, 0.82, 0.9, 16_000),
        reservation_hints: vec!["crates/grove-session/tests/phase2_acceptance.rs".to_owned()],
        parent_handoffs: vec![
            "Phase 2 implementation exists; acceptance now needs end-to-end evidence.".to_owned(),
        ],
        checkpoint: None,
        previous_failure_class: None,
        previous_outcome: None,
        rescue_card: None,
        retry_delta_summary: None,
        retrieval_query: None,
        token_budget: Some(2_000),
        ordinal_in_run: 1,
        archive_bundle: None,
        playbook_rules: Vec::new(),
        env: Vec::new(),
        shutdown: SessionShutdownConfig::default(),
    }
}

fn sample_previous_outcome(
    analysis: IterationAnalysis,
    terminal_class: SessionTerminalClass,
) -> SessionOutcome {
    SessionOutcome {
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
            transcript_path: ".grove/transcripts/grove-1j9.6.9/ses-prev.jsonl".to_owned(),
        },
        protocol_events: Vec::new(),
        analysis,
        terminal_class,
        context_pressure_pct: None,
        context_pressure_level: ContextPressureLevel::Ok,
        stdout_tail: Vec::new(),
        stderr_tail: Vec::new(),
    }
}

#[derive(Clone, Default)]
struct RecordingHooks {
    started: Arc<Mutex<Vec<ClaudeSessionRecord>>>,
    finished: Arc<Mutex<Vec<SessionOutcome>>>,
}

impl SessionLifecycleHooks for RecordingHooks {
    fn on_session_started(&mut self, session: &ClaudeSessionRecord) -> anyhow::Result<()> {
        self.started
            .lock()
            .expect("lock started hooks")
            .push(session.clone());
        Ok(())
    }

    fn on_session_finished(
        &mut self,
        result: &grove_session::SingleTaskSessionResult,
    ) -> anyhow::Result<()> {
        self.finished
            .lock()
            .expect("lock finished hooks")
            .push(result.outcome.clone());
        Ok(())
    }
}

struct FailingStartHooks;

impl SessionLifecycleHooks for FailingStartHooks {
    fn on_session_started(&mut self, _session: &ClaudeSessionRecord) -> anyhow::Result<()> {
        anyhow::bail!("persist start")
    }
}

struct FailingFinishHooks;

impl SessionLifecycleHooks for FailingFinishHooks {
    fn on_session_finished(
        &mut self,
        _result: &grove_session::SingleTaskSessionResult,
    ) -> anyhow::Result<()> {
        anyhow::bail!("persist finish")
    }
}

#[test]
fn lifecycle_hooks_observe_session_start_and_finish() -> TestResult {
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
            concat!(
                "working through the task\n",
                "GROVE_RESULT: session runner wired\n",
                "GROVE_EXIT: true\n",
                "all tasks complete\n",
                "implementation complete\n"
            )
            .to_owned(),
        ),
        ("STDERR_SCRIPT".to_owned(), String::new()),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let mut hooks = RecordingHooks::default();
    let result = execute_single_task_session_with_hooks(&backend, request, &mut hooks)?;

    let started = hooks.started.lock().expect("lock started assertions");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].status, SessionStatus::Running);
    assert_eq!(started[0].ended_at, None);
    assert_eq!(
        started[0].prompt_id.as_ref().map(|id| id.as_str()),
        Some("prompt-1")
    );

    let finished = hooks.finished.lock().expect("lock finished assertions");
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].session.status, SessionStatus::Completed);
    assert_eq!(
        finished[0].session.id.as_str(),
        result.outcome.session.id.as_str()
    );
    Ok(())
}

#[test]
fn started_then_runner_failure_still_emits_terminal_finish_callback() -> TestResult {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;

    let script_path = dir.path().join("missing-claude");
    let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let request = sample_request(workspace_dir);
    let mut hooks = RecordingHooks::default();
    let error = execute_single_task_session_with_hooks(&backend, request, &mut hooks)
        .expect_err("backend start should fail");
    assert!(matches!(
        error,
        grove_session::SingleTaskSessionRunnerError::BackendStart(_)
    ));

    let started = hooks.started.lock().expect("lock started assertions");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].status, SessionStatus::Running);

    let finished = hooks.finished.lock().expect("lock finished assertions");
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].session.status, SessionStatus::UnknownFailure);
    assert_eq!(
        finished[0].terminal_class,
        SessionTerminalClass::UnknownFailure
    );
    assert!(finished[0].session.ended_at.is_some());
    Ok(())
}

#[test]
fn lifecycle_start_hook_failure_surfaces_as_runner_error() -> TestResult {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir)
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let request = sample_request(workspace_dir);
    let error = execute_single_task_session_with_hooks(&backend, request, &mut FailingStartHooks)
        .expect_err("hook failure should bubble out");
    assert!(matches!(
        error,
        grove_session::SingleTaskSessionRunnerError::LifecycleHook(_)
    ));
    assert!(error.to_string().contains("persist start"));
    Ok(())
}

#[test]
fn successful_run_with_finish_hook_failure_surfaces_error() -> TestResult {
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
            concat!(
                "working through the task\n",
                "GROVE_RESULT: session runner wired\n",
                "GROVE_EXIT: true\n",
                "all tasks complete\n",
                "implementation complete\n"
            )
            .to_owned(),
        ),
        ("STDERR_SCRIPT".to_owned(), String::new()),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let error = execute_single_task_session_with_hooks(&backend, request, &mut FailingFinishHooks)
        .expect_err("finish hook failure should bubble out");
    assert!(matches!(
        error,
        grove_session::SingleTaskSessionRunnerError::LifecycleHook(_)
    ));
    assert!(error.to_string().contains("persist finish"));
    Ok(())
}

#[test]
fn one_task_success_path_persists_prompt_and_transcript_artifacts() -> TestResult {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir.clone())
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

    let prompt_path = workspace_dir.join(".grove/prompts/prompt-1.json");
    let transcript_path = workspace_dir.join(".grove/transcripts/grove-1j9.6.9/ses-1.jsonl");
    assert!(prompt_path.exists());
    assert!(transcript_path.exists());

    let replay = grove_session::replay_transcript(transcript_path.as_std_path())?;
    assert!(replay.events.len() >= 5);
    Ok(())
}

#[test]
fn success_requires_explicit_exit_and_indicator_threshold() -> TestResult {
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
            concat!(
                "still finishing verification\n",
                "GROVE_RESULT: not enough evidence yet\n",
                "GROVE_EXIT: true\n",
                "implementation complete\n"
            )
            .to_owned(),
        ),
        ("STDERR_SCRIPT".to_owned(), String::new()),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let result = execute_single_task_session(&backend, request)?;

    assert_eq!(
        result.outcome.terminal_class,
        SessionTerminalClass::UnknownFailure
    );
    assert_eq!(result.outcome.session.status, SessionStatus::UnknownFailure);
    assert!(result.outcome.analysis.has_explicit_exit_true);
    assert_eq!(result.outcome.analysis.completion_indicators, 1);
    Ok(())
}

#[test]
fn explicit_exit_false_overrides_later_success_markers() -> TestResult {
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
            concat!(
                "GROVE_EXIT: false\n",
                "continuing with follow-up work\n",
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

    assert_eq!(result.protocol_state.explicit_exit, Some(true));
    assert!(result.outcome.analysis.has_explicit_exit_true);
    assert!(result.outcome.analysis.has_explicit_exit_false);
    assert_eq!(result.outcome.analysis.completion_indicators, 2);
    assert_eq!(
        result.outcome.terminal_class,
        SessionTerminalClass::UnknownFailure
    );
    Ok(())
}

#[test]
fn checkpoint_takes_precedence_over_success_like_output() -> TestResult {
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
            concat!(
                "GROVE_RESULT: checkpoint before rotation\n",
                "GROVE_EXIT: true\n",
                "all tasks complete\n",
                "implementation complete\n",
                "GROVE_CHECKPOINT: {\"progress\":\"halfway\",\"next_step\":\"finish wiring\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[\"crates/grove-session/src/**\"]}\n"
            )
            .to_owned(),
        ),
        ("STDERR_SCRIPT".to_owned(), String::new()),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let result = execute_single_task_session(&backend, request)?;

    assert_eq!(
        result.outcome.terminal_class,
        SessionTerminalClass::Checkpoint
    );
    assert_eq!(result.outcome.session.status, SessionStatus::Checkpointed);
    assert_eq!(
        result.outcome.session.stop_reason,
        Some(StopReason::Checkpoint)
    );
    assert_eq!(
        result
            .protocol_state
            .latest_checkpoint
            .as_ref()
            .map(|payload| payload.next_step.as_str()),
        Some("finish wiring")
    );
    Ok(())
}

#[test]
fn permission_denied_preempts_generic_crash_classification() -> TestResult {
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
            "tool use rejected by policy\n".to_owned(),
        ),
        (
            "STDERR_SCRIPT".to_owned(),
            "permission denied opening sandboxed path\n".to_owned(),
        ),
        ("EXIT_CODE".to_owned(), "1".to_owned()),
    ];

    let result = execute_single_task_session(&backend, request)?;

    assert_eq!(result.outcome.analysis.permission_denials, 2);
    assert_eq!(
        result.outcome.terminal_class,
        SessionTerminalClass::PermissionDenied
    );
    assert_eq!(
        result.outcome.session.status,
        SessionStatus::PermissionDenied
    );
    assert_eq!(
        result.outcome.session.stop_reason,
        Some(StopReason::PermissionDenied)
    );
    Ok(())
}

#[test]
fn rate_limit_preempts_generic_crash_classification() -> TestResult {
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
        result.outcome.session.stop_reason,
        Some(StopReason::RateLimit)
    );
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

#[test]
fn repeated_error_retry_context_uses_retry_rescue_contract_and_persists_manifest_details()
-> TestResult {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir.clone())
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let previous_outcome = sample_previous_outcome(
        IterationAnalysis {
            probable_progress: ProgressSignal::Weak,
            repeated_error_fingerprint: Some(
                "error: protocol marker was malformed and failed to parse".to_owned(),
            ),
            has_explicit_exit_false: true,
            ..IterationAnalysis::default()
        },
        SessionTerminalClass::UnknownFailure,
    );

    let mut request = sample_request(workspace_dir.clone())
        .with_retry_context(FailureClass::RepeatedError, Some(previous_outcome));
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
    assert!(request.rescue_card.as_deref().is_some_and(|card| {
        card.contains("Previous repeated error to avoid") && card.contains("`GROVE_EXIT: false`")
    }));

    request.env = vec![
        (
            "STDOUT_SCRIPT".to_owned(),
            concat!(
                "retry attempt with changed approach\n",
                "GROVE_RESULT: retry mutation applied\n",
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
    assert!(rescue_card.included);
    assert!(
        rescue_card
            .preview
            .contains("Do not repeat the same failing path")
    );
    assert_eq!(
        result.protocol_state.result_summary.as_deref(),
        Some("retry mutation applied")
    );
    assert_eq!(result.outcome.terminal_class, SessionTerminalClass::Success);
    Ok(())
}

#[test]
fn interrupted_retry_context_uses_resume_contract_and_persists_manifest_details() -> TestResult {
    let dir = tempdir()?;
    let workspace_dir = dir.path().join("workspace");
    fs::create_dir_all(&workspace_dir)?;
    let workspace_dir = Utf8PathBuf::from_path_buf(workspace_dir.clone())
        .map_err(|_| io::Error::other("workspace dir must be valid UTF-8"))?;

    let script_path = dir.path().join("fake-claude");
    write_fake_claude_script(&script_path)?;
    let backend = CliClaudeBackend::new(script_path.to_string_lossy().into_owned());

    let previous_outcome = sample_previous_outcome(
        IterationAnalysis {
            probable_progress: ProgressSignal::Strong,
            ..IterationAnalysis::default()
        },
        SessionTerminalClass::Crash,
    );

    let mut request = sample_request(workspace_dir.clone())
        .with_retry_context(FailureClass::Interrupted, Some(previous_outcome));
    assert_eq!(
        request.previous_failure_class,
        Some(FailureClass::Interrupted)
    );
    assert_eq!(request.contract, ExecutionContract::Resume);
    assert!(
        request
            .retry_delta_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("resumes from durable progress"))
    );
    assert!(
        request
            .rescue_card
            .as_deref()
            .is_some_and(|card| card.contains("Resume from the first unfinished step only"))
    );

    request.env = vec![
        (
            "STDOUT_SCRIPT".to_owned(),
            concat!(
                "resuming from durable progress\n",
                "GROVE_RESULT: resumed safely\n",
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

    let prompt_path = workspace_dir.join(".grove/prompts/prompt-1.json");
    let manifest: PromptManifest =
        serde_json::from_str(&fs::read_to_string(prompt_path.as_std_path())?)?;
    assert_eq!(manifest.contract, ExecutionContract::Resume);
    assert!(
        manifest
            .retry_delta_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("resumes from durable progress"))
    );
    let rescue_card = manifest
        .sections
        .iter()
        .find(|section| section.kind == PromptSegmentKind::RescueCard)
        .ok_or("missing rescue-card section")?;
    assert!(
        rescue_card
            .preview
            .contains("Resume from the first unfinished step only")
    );
    assert_eq!(result.outcome.terminal_class, SessionTerminalClass::Success);
    assert_eq!(result.outcome.session.status, SessionStatus::Completed);
    Ok(())
}

#[test]
fn lifecycle_hooks_expose_started_record_fields_needed_for_persistence() -> TestResult {
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
            concat!(
                "working through the task\n",
                "GROVE_RESULT: session runner wired\n",
                "GROVE_EXIT: true\n",
                "all tasks complete\n",
                "implementation complete\n"
            )
            .to_owned(),
        ),
        ("STDERR_SCRIPT".to_owned(), String::new()),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let mut hooks = RecordingHooks::default();
    execute_single_task_session_with_hooks(&backend, request, &mut hooks)?;

    let started = hooks.started.lock().expect("lock started assertions");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].ordinal_in_run, 1);
    assert_eq!(
        started[0].prompt_manifest_path.as_deref(),
        Some(".grove/prompts/prompt-1.json")
    );
    assert_eq!(
        started[0].transcript_path,
        ".grove/transcripts/grove-1j9.6.9/ses-1.jsonl"
    );
    assert!(started[0].prompt_bytes > 0);
    assert!(started[0].estimated_input_tokens > 0);
    Ok(())
}

#[test]
fn lifecycle_hooks_observe_checkpoint_terminal_outcome_and_payload() -> TestResult {
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
            concat!(
                "GROVE_RESULT: checkpoint before rotation\n",
                "GROVE_EXIT: true\n",
                "all tasks complete\n",
                "implementation complete\n",
                "GROVE_CHECKPOINT: {\"progress\":\"halfway\",\"next_step\":\"finish wiring\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[\"crates/grove-session/src/**\"]}\n"
            )
            .to_owned(),
        ),
        ("STDERR_SCRIPT".to_owned(), String::new()),
        ("EXIT_CODE".to_owned(), "0".to_owned()),
    ];

    let mut hooks = RecordingHooks::default();
    let result = execute_single_task_session_with_hooks(&backend, request, &mut hooks)?;

    let finished = hooks.finished.lock().expect("lock finished assertions");
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].terminal_class, SessionTerminalClass::Checkpoint);
    assert_eq!(finished[0].session.status, SessionStatus::Checkpointed);
    assert_eq!(
        finished[0].session.stop_reason,
        Some(StopReason::Checkpoint)
    );
    assert!(finished[0].session.ended_at.is_some());
    assert_eq!(
        result
            .protocol_state
            .latest_checkpoint
            .as_ref()
            .map(|payload| payload.next_step.as_str()),
        Some("finish wiring")
    );
    Ok(())
}
