#![cfg(unix)]

use std::{
    error::Error,
    fs,
    io,
    os::unix::fs::PermissionsExt,
    time::Duration,
};

use camino::Utf8PathBuf;
use chrono::Utc;
use grove_session::{
    CliClaudeBackend, ContextMonitor, ExitPolicy, SingleTaskSessionRequest,
    execute_single_task_session,
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
        token_budget: Some(2_000),
        ordinal_in_run: 1,
        env: Vec::new(),
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

    assert_eq!(result.outcome.terminal_class, SessionTerminalClass::UnknownFailure);
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
    assert_eq!(result.outcome.terminal_class, SessionTerminalClass::UnknownFailure);
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

    assert_eq!(result.outcome.terminal_class, SessionTerminalClass::Checkpoint);
    assert_eq!(result.outcome.session.status, SessionStatus::Checkpointed);
    assert_eq!(result.outcome.session.stop_reason, Some(StopReason::Checkpoint));
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
    assert_eq!(result.outcome.terminal_class, SessionTerminalClass::PermissionDenied);
    assert_eq!(result.outcome.session.status, SessionStatus::PermissionDenied);
    assert_eq!(result.outcome.session.stop_reason, Some(StopReason::PermissionDenied));
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

    let mut request =
        sample_request(workspace_dir.clone()).with_retry_context(FailureClass::Interrupted, Some(previous_outcome));
    assert_eq!(request.contract, ExecutionContract::Resume);
    assert!(request
        .retry_delta_summary
        .as_deref()
        .is_some_and(|summary| summary.contains("resumes from durable progress")));

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
    assert!(manifest
        .retry_delta_summary
        .as_deref()
        .is_some_and(|summary| summary.contains("resumes from durable progress")));
    let rescue_card = manifest
        .sections
        .iter()
        .find(|section| section.kind == PromptSegmentKind::RescueCard)
        .ok_or("missing rescue-card section")?;
    assert!(rescue_card.preview.contains("Resume from the first unfinished step only"));
    assert_eq!(result.outcome.terminal_class, SessionTerminalClass::Success);
    assert_eq!(result.outcome.session.status, SessionStatus::Completed);
    Ok(())
}
