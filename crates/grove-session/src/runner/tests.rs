
use super::*;
use crate::{CliClaudeBackend, replay_transcript};
use grove_types::{
    FailureClass, IterationAnalysis, ProgressSignal, PromptManifest, PromptSegmentKind,
    RuntimeProvider,
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
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, script)?;
    let mut permissions = fs::metadata(&temp_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&temp_path, permissions)?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

#[cfg(unix)]
fn sample_request(workspace_dir: Utf8PathBuf) -> SingleTaskSessionRequest {
    SingleTaskSessionRequest {
        bead_id: BeadId::new("grove-1j9.6.8"),
        run_id: RunId::new("run-1"),
        session_id: SessionId::new("ses-1"),
        provider: RuntimeProvider::Claude,
        prompt_id: PromptId::new("prompt-1"),
        task_title: "Implement single task runner".to_owned(),
        task_description: "Wire the session subsystem end to end.".to_owned(),
        startup_prompt: None,
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
        escalation_tier: EscalationTier::FirstAttempt,
        mutation_strategy: None,
        idle_grace_period: Duration::from_secs(300),
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
            provider: RuntimeProvider::Claude,
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
