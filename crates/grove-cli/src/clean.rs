use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};
use camino::{Utf8Path, Utf8PathBuf};
use grove_config::{LoadedConfig, build_provider_environment};
use grove_db::{Database, RecoveryCapsuleEvent};
use grove_kernel::{
    ExecutionPolicyAction, PolicyVerdict, ProviderAction, evaluate_execution_policy,
    lesson_ingest::ingest_lessons, trace_runtime_event,
};
use grove_session::build_provider_cli_args;
use grove_types::{
    BeadId, CheckpointRecord, ClaudeSessionRecord, CleanupSnapshotRecord, GroveBeadRecord,
    HandoffRecord, RunStatus, SessionStatus, TaskRunRecord,
};
use serde::{Deserialize, Serialize};

const DEFAULT_LOG_KEEP_LINES: usize = 10_000;

#[derive(Debug, Clone, Serialize)]
pub struct CleanReport {
    pub workspace_root: String,
    pub dry_run: bool,
    pub candidates: Vec<CandidateReport>,
    pub skipped: Vec<SkippedReport>,
    pub log_compaction: Option<LogCompactionReport>,
    pub total_deleted_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateReport {
    pub bead_id: String,
    pub run_id: String,
    pub session_id: String,
    pub deleted_bytes: i64,
    pub cleaned_artifacts: Vec<ArtifactPlan>,
    pub cleanup_snapshot_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedReport {
    pub bead_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogCompactionReport {
    pub path: String,
    pub previous_bytes: i64,
    pub deleted_bytes: i64,
    pub line_count: usize,
    pub kept_lines: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactPlan {
    pub kind: String,
    pub path: String,
    pub bytes: i64,
}

#[derive(Debug, Clone)]
struct CleanupCandidate {
    bead: GroveBeadRecord,
    latest_run: TaskRunRecord,
    latest_session: ClaudeSessionRecord,
    latest_handoff: Option<HandoffRecord>,
    latest_checkpoint: Option<CheckpointRecord>,
    latest_recovery_capsule: Option<RecoveryCapsuleEvent>,
    artifacts: Vec<ArtifactPlan>,
}

#[derive(Debug, Clone, Serialize)]
struct CleanupPromptInput {
    bead_id: String,
    bead_title: String,
    run_id: String,
    session_id: String,
    handoff: Option<HandoffRecord>,
    checkpoint: Option<CheckpointRecord>,
    recovery_capsule: Option<RecoveryCapsuleEvent>,
    prompt_manifest: Option<serde_json::Value>,
    transcript_tail: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CleanupSynthesis {
    continuity_summary: String,
    next_bead_guidance: String,
    lessons: Vec<String>,
    decisions: Vec<String>,
    warnings: Vec<String>,
    prompt_summary: String,
    transcript_tail_summary: String,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CleanOptions<'a> {
    pub bead_filter: Option<&'a BeadId>,
    pub dry_run: bool,
    pub no_llm: bool,
    pub max_beads: Option<usize>,
    pub json_mode: bool,
    pub yes: bool,
}

pub(super) fn handle_clean(
    loaded: LoadedConfig,
    mut db: Database,
    options: CleanOptions<'_>,
) -> Result<()> {
    let clean_policy = evaluate_execution_policy(&ExecutionPolicyAction::WorkspaceClean {
        dry_run: options.dry_run,
        confirmed: options.yes,
    });
    if !clean_policy.verdict.permits_execution() {
        bail!("{}", clean_policy.reason);
    }

    let report = execute_clean(
        &loaded,
        &mut db,
        options.bead_filter,
        options.dry_run,
        options.no_llm,
        options.max_beads,
    )?;

    if options.json_mode {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }

    Ok(())
}

pub(super) fn compact_summary_lines(snapshot: &CleanupSnapshotRecord) -> Vec<String> {
    let mut lines = vec!["Transcript was cleaned by `grove clean`.".to_owned()];
    if !snapshot.transcript_tail_summary.trim().is_empty() {
        lines.push(snapshot.transcript_tail_summary.trim().to_owned());
    }
    if !snapshot.continuity_summary.trim().is_empty() {
        lines.push(format!(
            "Continuity summary: {}",
            snapshot.continuity_summary.trim()
        ));
    }
    lines
}

fn execute_clean(
    loaded: &LoadedConfig,
    db: &mut Database,
    bead_filter: Option<&BeadId>,
    dry_run: bool,
    no_llm: bool,
    max_beads: Option<usize>,
) -> Result<CleanReport> {
    let pending_mirror_beads = db
        .list_pending_mirror_operations(10_000)?
        .into_iter()
        .map(|operation| operation.bead_id)
        .collect::<HashSet<_>>();

    let mut candidates = Vec::new();
    let mut skipped = Vec::new();

    let mut bead_records = db.list_bead_records()?;
    bead_records.sort_by_key(|record| record.runtime_updated_at);

    for bead in bead_records {
        if let Some(filter) = bead_filter
            && &bead.bead.id != filter
        {
            continue;
        }

        match candidate_for_bead(loaded, db, bead, &pending_mirror_beads)? {
            CandidateState::Eligible(candidate) => candidates.push(*candidate),
            CandidateState::Skipped(reason, bead_id) => skipped.push(SkippedReport {
                bead_id: bead_id.to_string(),
                reason,
            }),
        }
    }

    if let Some(limit) = max_beads {
        candidates.truncate(limit);
    }

    let log_compaction = runtime_log_plan(
        loaded.paths.workspace_root(),
        loaded.paths.logs_dir().join("runtime.jsonl"),
    )?;
    let mut candidate_reports = Vec::new();
    let mut total_deleted_bytes = 0_i64;

    for candidate in candidates {
        let deleted_bytes = candidate
            .artifacts
            .iter()
            .map(|artifact| artifact.bytes)
            .sum();
        if dry_run {
            candidate_reports.push(CandidateReport {
                bead_id: candidate.bead.bead.id.to_string(),
                run_id: candidate.latest_run.id.to_string(),
                session_id: candidate.latest_session.id.to_string(),
                deleted_bytes,
                cleaned_artifacts: candidate.artifacts.clone(),
                cleanup_snapshot_id: None,
            });
            total_deleted_bytes += deleted_bytes;
            continue;
        }

        let synthesis = if no_llm {
            deterministic_cleanup_synthesis(loaded, &candidate)?
        } else {
            match synthesize_cleanup_snapshot(loaded, &candidate)
                .with_context(|| format!("compact bead {}", candidate.bead.bead.id))
            {
                Ok(synthesis) => synthesis,
                Err(error) => {
                    skipped.push(SkippedReport {
                        bead_id: candidate.bead.bead.id.to_string(),
                        reason: error.to_string(),
                    });
                    continue;
                }
            }
        };

        if !synthesis.lessons.is_empty() {
            let _ = ingest_lessons(
                db,
                &candidate.bead.bead.id,
                &candidate.latest_run.id,
                &synthesis.lessons,
            );
        }

        let now = chrono::Utc::now();
        let snapshot_id = format!(
            "clean-{}-{}",
            candidate.latest_session.id.as_str(),
            now.format("%Y%m%dT%H%M%S%.3f")
        );
        let snapshot = CleanupSnapshotRecord {
            id: snapshot_id.clone(),
            bead_id: candidate.bead.bead.id.clone(),
            run_id: candidate.latest_run.id.clone(),
            session_id: candidate.latest_session.id.clone(),
            provider: loaded.config.runtime.provider.as_str().to_owned(),
            model: loaded.config.runtime.default_model.clone(),
            cleaned_artifact_paths: candidate
                .artifacts
                .iter()
                .map(|artifact| artifact.path.clone())
                .collect(),
            cleaned_artifact_kinds: candidate
                .artifacts
                .iter()
                .map(|artifact| artifact.kind.clone())
                .collect(),
            deleted_bytes,
            continuity_summary: synthesis.continuity_summary.clone(),
            next_bead_guidance: synthesis.next_bead_guidance.clone(),
            lessons: synthesis.lessons.clone(),
            decisions: synthesis.decisions.clone(),
            warnings: synthesis.warnings.clone(),
            prompt_summary: synthesis.prompt_summary.clone(),
            transcript_tail_summary: synthesis.transcript_tail_summary.clone(),
            created_at: now,
        };
        db.insert_cleanup_snapshot(&snapshot)?;

        if candidate
            .latest_handoff
            .as_ref()
            .is_none_or(|handoff| handoff.run_id != candidate.latest_run.id)
        {
            db.write_handoff(grove_db::HandoffWriteInput {
                bead_id: candidate.bead.bead.id.clone(),
                run_id: candidate.latest_run.id.clone(),
                summary: synthesis.continuity_summary.clone(),
                artifacts: candidate
                    .latest_handoff
                    .as_ref()
                    .map(|handoff| handoff.artifacts.clone())
                    .unwrap_or_default(),
                lessons: synthesis.lessons.clone(),
                decisions: synthesis.decisions.clone(),
                warnings: synthesis.warnings.clone(),
                completed_at: now,
            })?;
        }

        for artifact in &candidate.artifacts {
            let absolute = resolve_workspace_path(loaded.paths.workspace_root(), &artifact.path);
            if absolute.is_file() {
                fs::remove_file(&absolute)
                    .with_context(|| format!("remove {}", absolute.display()))?;
            }
            remove_empty_parent_dir(loaded.paths.transcript_dir(), &absolute);
            remove_empty_parent_dir(&loaded.paths.checkpoints_dir(), &absolute);
        }

        candidate_reports.push(CandidateReport {
            bead_id: candidate.bead.bead.id.to_string(),
            run_id: candidate.latest_run.id.to_string(),
            session_id: candidate.latest_session.id.to_string(),
            deleted_bytes,
            cleaned_artifacts: candidate.artifacts.clone(),
            cleanup_snapshot_id: Some(snapshot_id),
        });
        total_deleted_bytes += deleted_bytes;
    }

    let log_compaction = match log_compaction {
        Some(plan) if dry_run => {
            total_deleted_bytes += plan.deleted_bytes;
            Some(plan)
        }
        Some(plan) => {
            compact_runtime_log(resolve_workspace_path(
                loaded.paths.workspace_root(),
                &plan.path,
            ))?;
            total_deleted_bytes += plan.deleted_bytes;
            Some(plan)
        }
        None => None,
    };

    Ok(CleanReport {
        workspace_root: loaded.paths.workspace_root().to_string(),
        dry_run,
        candidates: candidate_reports,
        skipped,
        log_compaction,
        total_deleted_bytes,
    })
}

enum CandidateState {
    Eligible(Box<CleanupCandidate>),
    Skipped(String, BeadId),
}

fn candidate_for_bead(
    loaded: &LoadedConfig,
    db: &Database,
    bead: GroveBeadRecord,
    pending_mirror_beads: &HashSet<BeadId>,
) -> Result<CandidateState> {
    let bead_id = bead.bead.id.clone();
    if bead.grove_status != grove_types::GroveBeadStatus::Succeeded {
        return Ok(CandidateState::Skipped(
            format!("grove status is {:?}", bead.grove_status),
            bead_id,
        ));
    }
    if pending_mirror_beads.contains(&bead.bead.id) {
        return Ok(CandidateState::Skipped(
            "mirror outbox still has pending work".to_owned(),
            bead_id,
        ));
    }

    let runs = db.list_task_runs_for_bead(&bead.bead.id)?;
    let Some(latest_run) = runs.into_iter().next() else {
        return Ok(CandidateState::Skipped(
            "no task runs recorded".to_owned(),
            bead_id,
        ));
    };
    if latest_run.status != RunStatus::Succeeded {
        return Ok(CandidateState::Skipped(
            format!("latest run status is {:?}", latest_run.status),
            bead_id,
        ));
    }
    let Some(latest_session) = db.latest_session_for_run(&latest_run.id)? else {
        return Ok(CandidateState::Skipped(
            "latest run has no session".to_owned(),
            bead_id,
        ));
    };
    if latest_session.status != SessionStatus::Completed {
        return Ok(CandidateState::Skipped(
            format!("latest session status is {:?}", latest_session.status),
            bead_id,
        ));
    }

    let latest_handoff = db.handoff_for_bead(&bead.bead.id)?;
    let latest_checkpoint = db.latest_checkpoint_for_bead(&bead.bead.id)?;
    let latest_recovery_capsule = db.latest_recovery_capsule_for_bead(&bead.bead.id)?;

    let mut artifacts = Vec::new();
    let transcript_path = resolve_workspace_path(
        loaded.paths.workspace_root(),
        &latest_session.transcript_path,
    );
    if transcript_path.is_file() {
        let transcript_ingested =
            db.is_session_ingested("transcript", latest_session.id.as_str())?;
        if !transcript_ingested {
            return Ok(CandidateState::Skipped(
                "transcript has not been archived yet".to_owned(),
                bead_id,
            ));
        }
        artifacts.push(ArtifactPlan {
            kind: "transcript".to_owned(),
            path: latest_session.transcript_path.clone(),
            bytes: file_len(&transcript_path)?,
        });
    }

    if let Some(prompt_manifest_path) = latest_session.prompt_manifest_path.as_deref() {
        let prompt_path =
            resolve_workspace_path(loaded.paths.workspace_root(), prompt_manifest_path);
        if prompt_path.is_file() {
            artifacts.push(ArtifactPlan {
                kind: "prompt_manifest".to_owned(),
                path: prompt_manifest_path.to_owned(),
                bytes: file_len(&prompt_path)?,
            });
        }
    }

    let checkpoints_dir = loaded.paths.checkpoints_dir().join(bead.bead.id.as_str());
    if checkpoints_dir.is_dir() {
        for entry in fs::read_dir(checkpoints_dir.as_std_path())
            .with_context(|| format!("read checkpoints for {}", bead.bead.id))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let path = Utf8PathBuf::from_path_buf(path).map_err(|path| {
                    anyhow!("checkpoint path was not valid UTF-8: {}", path.display())
                })?;
                let relative = path
                    .strip_prefix(loaded.paths.workspace_root())
                    .unwrap_or(path.as_path())
                    .to_string();
                artifacts.push(ArtifactPlan {
                    kind: "checkpoint".to_owned(),
                    path: relative,
                    bytes: file_len(path.as_std_path())?,
                });
            }
        }
    }

    if artifacts.is_empty() {
        return Ok(CandidateState::Skipped(
            "no eligible files remain for cleanup".to_owned(),
            bead_id,
        ));
    }

    Ok(CandidateState::Eligible(Box::new(CleanupCandidate {
        bead,
        latest_run,
        latest_session,
        latest_handoff,
        latest_checkpoint,
        latest_recovery_capsule,
        artifacts,
    })))
}

fn synthesize_cleanup_snapshot(
    loaded: &LoadedConfig,
    candidate: &CleanupCandidate,
) -> Result<CleanupSynthesis> {
    let prompt_manifest = candidate
        .latest_session
        .prompt_manifest_path
        .as_deref()
        .and_then(|path| {
            let resolved = resolve_workspace_path(loaded.paths.workspace_root(), path);
            fs::read_to_string(resolved)
                .ok()
                .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        });
    let transcript_path = resolve_workspace_path(
        loaded.paths.workspace_root(),
        &candidate.latest_session.transcript_path,
    );
    let transcript_tail =
        super::read_transcript_tail(&transcript_path.to_string_lossy(), 40)?.unwrap_or_default();

    let input = CleanupPromptInput {
        bead_id: candidate.bead.bead.id.to_string(),
        bead_title: candidate.bead.bead.title.clone(),
        run_id: candidate.latest_run.id.to_string(),
        session_id: candidate.latest_session.id.to_string(),
        handoff: candidate.latest_handoff.clone(),
        checkpoint: candidate.latest_checkpoint.clone(),
        recovery_capsule: candidate.latest_recovery_capsule.clone(),
        prompt_manifest,
        transcript_tail,
    };

    let prompt = format!(
        "You are compacting Grove session history before raw files are deleted.\n\
Return exactly one JSON object and nothing else.\n\
The JSON schema is:\n\
{{\"continuity_summary\":string,\"next_bead_guidance\":string,\"lessons\":string[],\"decisions\":string[],\"warnings\":string[],\"prompt_summary\":string,\"transcript_tail_summary\":string}}\n\
Keep summaries compact and concrete. Preserve next-bead continuity, not full forensics.\n\
Input:\n{}\n",
        serde_json::to_string_pretty(&input)?
    );

    run_provider_compaction(loaded, &prompt)
}

fn deterministic_cleanup_synthesis(
    loaded: &LoadedConfig,
    candidate: &CleanupCandidate,
) -> Result<CleanupSynthesis> {
    let transcript_path = resolve_workspace_path(
        loaded.paths.workspace_root(),
        &candidate.latest_session.transcript_path,
    );
    let transcript_tail =
        super::read_transcript_tail(&transcript_path.to_string_lossy(), 20)?.unwrap_or_default();
    let continuity_summary = candidate
        .latest_handoff
        .as_ref()
        .map(|handoff| handoff.summary.clone())
        .or_else(|| {
            candidate
                .latest_recovery_capsule
                .as_ref()
                .map(|capsule| capsule.capsule.summary.clone())
        })
        .unwrap_or_else(|| {
            format!(
                "Completed bead {} and cleaned raw session artifacts.",
                candidate.bead.bead.id
            )
        });
    let next_bead_guidance = candidate
        .latest_checkpoint
        .as_ref()
        .map(|checkpoint| checkpoint.next_step.clone())
        .or_else(|| {
            candidate
                .latest_handoff
                .as_ref()
                .and_then(|handoff| handoff.decisions.first().cloned())
        })
        .unwrap_or_else(|| {
            "Use the durable handoff, playbook bullets, and archive snippets for follow-on work."
                .to_owned()
        });
    let lessons = candidate
        .latest_handoff
        .as_ref()
        .map(|handoff| handoff.lessons.clone())
        .unwrap_or_default();
    let decisions = candidate
        .latest_handoff
        .as_ref()
        .map(|handoff| handoff.decisions.clone())
        .unwrap_or_default();
    let warnings = candidate
        .latest_handoff
        .as_ref()
        .map(|handoff| handoff.warnings.clone())
        .unwrap_or_default();
    let prompt_summary = candidate
        .latest_session
        .prompt_manifest_path
        .as_ref()
        .map(|path| format!("Prompt manifest stored at {path} before cleanup."))
        .unwrap_or_else(|| "No prompt manifest was recorded for the latest session.".to_owned());
    let transcript_tail_summary = if transcript_tail.is_empty() {
        "No transcript tail was available before cleanup.".to_owned()
    } else {
        transcript_tail.join(" | ")
    };

    Ok(CleanupSynthesis {
        continuity_summary,
        next_bead_guidance,
        lessons,
        decisions,
        warnings,
        prompt_summary,
        transcript_tail_summary,
    })
}

fn run_provider_compaction(loaded: &LoadedConfig, prompt: &str) -> Result<CleanupSynthesis> {
    let current_env = env::vars().collect::<HashMap<_, _>>();
    let provider = loaded.config.runtime.provider;
    let init_args = loaded.config.runtime.effective_init_args();
    let launch_policy = evaluate_execution_policy(&ExecutionPolicyAction::ProviderLaunch {
        provider,
        provider_bin: loaded.config.runtime.provider_bin.clone(),
        init_args: init_args.clone(),
        action: ProviderAction::CleanupCompaction,
    });
    if !launch_policy.verdict.permits_execution() {
        bail!("{}", launch_policy.reason);
    }
    if matches!(launch_policy.verdict, PolicyVerdict::AllowWithEscalation) {
        trace_runtime_event(
            "policy.provider_launch_escalated",
            serde_json::json!({
                "action": "cleanup_compaction",
                "provider": provider.as_str(),
                "provider_bin": loaded.config.runtime.provider_bin,
                "reason": launch_policy.reason,
            }),
        );
    }
    let env_vars = build_provider_environment(provider, &loaded.config.runtime, &current_env);

    let mut command = Command::new(&loaded.config.runtime.provider_bin);
    command.args(build_provider_cli_args(
        provider,
        &init_args,
        &loaded.config.runtime.default_model,
        prompt,
    ));

    command.current_dir(loaded.paths.workspace_root().as_std_path());
    for (key, value) in env_vars {
        command.env(key, value);
    }

    let output = command
        .output()
        .with_context(|| format!("spawn {}", loaded.config.runtime.provider_bin))?;
    if !output.status.success() {
        bail!(
            "provider compaction failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    parse_cleanup_synthesis(&String::from_utf8_lossy(&output.stdout))
}

fn parse_cleanup_synthesis(stdout: &str) -> Result<CleanupSynthesis> {
    for line in stdout.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<CleanupSynthesis>(trimmed) {
            return Ok(value);
        }
    }

    let start = stdout.find('{');
    let end = stdout.rfind('}');
    if let (Some(start), Some(end)) = (start, end) {
        let candidate = &stdout[start..=end];
        if let Ok(value) = serde_json::from_str::<CleanupSynthesis>(candidate) {
            return Ok(value);
        }
    }

    Err(anyhow!(
        "provider output did not contain a valid cleanup JSON object"
    ))
}

fn runtime_log_plan(
    workspace_root: &Utf8Path,
    path: Utf8PathBuf,
) -> Result<Option<LogCompactionReport>> {
    if !path.is_file() {
        return Ok(None);
    }
    let contents = fs::read_to_string(path.as_std_path())
        .with_context(|| format!("read runtime log {}", path))?;
    let lines = contents.lines().count();
    if lines <= DEFAULT_LOG_KEEP_LINES {
        return Ok(None);
    }
    let previous_bytes = contents.len() as i64;
    let kept = contents
        .lines()
        .rev()
        .take(DEFAULT_LOG_KEEP_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let deleted_bytes = previous_bytes - kept.len() as i64;
    Ok(Some(LogCompactionReport {
        path: path
            .strip_prefix(workspace_root)
            .unwrap_or(path.as_path())
            .to_string(),
        previous_bytes,
        deleted_bytes,
        line_count: lines,
        kept_lines: DEFAULT_LOG_KEEP_LINES,
    }))
}

fn compact_runtime_log(path: PathBuf) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("read runtime log {}", path.display()))?;
    let lines = contents.lines().count();
    if lines <= DEFAULT_LOG_KEEP_LINES {
        return Ok(());
    }
    let mut kept = contents
        .lines()
        .rev()
        .take(DEFAULT_LOG_KEEP_LINES)
        .collect::<Vec<_>>();
    kept.reverse();
    let mut rendered = kept.join("\n");
    if contents.ends_with('\n') {
        rendered.push('\n');
    }
    fs::write(&path, rendered).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn remove_empty_parent_dir(root: &Utf8Path, path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if parent == root.as_std_path() {
        return;
    }
    if fs::read_dir(parent)
        .ok()
        .is_some_and(|mut entries| entries.next().is_none())
    {
        let _ = fs::remove_dir(parent);
    }
}

fn file_len(path: &Path) -> Result<i64> {
    Ok(fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len() as i64)
}

fn resolve_workspace_path(workspace_root: &Utf8Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        workspace_root.as_std_path().join(path)
    }
}

fn print_report(report: &CleanReport) {
    println!("Workspace: {}", report.workspace_root);
    println!("Dry run: {}", report.dry_run);
    println!("Total reclaimable bytes: {}", report.total_deleted_bytes);

    println!("\nCandidates:");
    if report.candidates.is_empty() {
        println!("- none");
    } else {
        for candidate in &report.candidates {
            println!(
                "- {} run {} session {} reclaim {} bytes",
                candidate.bead_id, candidate.run_id, candidate.session_id, candidate.deleted_bytes
            );
            for artifact in &candidate.cleaned_artifacts {
                println!(
                    "  - {} {} ({} bytes)",
                    artifact.kind, artifact.path, artifact.bytes
                );
            }
            if let Some(snapshot_id) = candidate.cleanup_snapshot_id.as_deref() {
                println!("  - cleanup snapshot: {snapshot_id}");
            }
        }
    }

    println!("\nSkipped:");
    if report.skipped.is_empty() {
        println!("- none");
    } else {
        for skipped in &report.skipped {
            println!("- {}: {}", skipped.bead_id, skipped.reason);
        }
    }

    println!("\nRuntime log:");
    match report.log_compaction.as_ref() {
        Some(log) => {
            println!(
                "- {} reclaim {} bytes ({} lines -> keep {})",
                log.path, log.deleted_bytes, log.line_count, log.kept_lines
            );
        }
        None => println!("- no compaction needed"),
    }
}
