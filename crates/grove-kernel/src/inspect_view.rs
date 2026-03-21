use crate::status_view::{
    DispatchExplanationView, MirrorPendingView, ReservationConflictView, ScoreComponentView,
    conflicts_for_bead, find_reservation_conflicts, latest_mirror_pending_for_bead,
    ready_age_minutes, triage_context_for_bead,
};
use crate::{DispatchEligibilityContext, evaluate_dispatch_eligibility};
use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use grove_br::BrClient;
use grove_bv::BvTriageOutput;
use grove_config::GroveConfig;
use grove_db::{Database, RecoveryCapsuleEvent};
use grove_types::{
    BeadId, BulletId, CheckpointRecord, ClaudeSessionRecord, EventLogRecord, GroveBeadRecord,
    GroveBeadStatus, HandoffRecord, PlaybookBulletRecord, PromptManifest, RecoveryCapsule,
    RecoveryCapsuleOutcome, RelevantSnippet, RetrievalBundle, RunId, SessionOutcome, TaskRunRecord,
    Timestamp, DispatchDecisionRecord, PromptMaterializationRecord,
};
use std::fs;

pub const QUERY_PURPOSE: &str =
    "Operator-facing inspect query models for grove inspect bead diagnostics.";

pub fn load_inspect_snapshot<C: BrClient>(
    db: &Database,
    br: &C,
    bead_id: &BeadId,
    workspace_root: &str,
    config: &GroveConfig,
    triage: Option<&BvTriageOutput>,
) -> Result<Option<InspectSnapshot>> {
    let Some(bead) = db.get_bead_record(bead_id)? else {
        return Ok(None);
    };

    let ready_ids = br
        .ready()?
        .into_iter()
        .map(|summary| summary.id)
        .collect::<std::collections::HashSet<_>>();
    let dependency_snapshot = db.dependency_snapshot(bead_id)?;
    let all_beads = db.list_bead_records()?;
    let bead_index = all_beads
        .iter()
        .map(|record| (record.bead.id.clone(), record))
        .collect::<std::collections::HashMap<_, _>>();
    let reservations = db.list_active_reservations()?;
    let reservation_conflicts = find_reservation_conflicts(&reservations);
    let bead_conflicts = conflicts_for_bead(bead_id, &reservation_conflicts);
    let ready_in_br = ready_ids.contains(bead_id);
    let now = Utc::now();
    let eligibility = evaluate_dispatch_eligibility(
        &bead,
        &DispatchEligibilityContext {
            ready_in_br,
            circuit_state: grove_types::CircuitState::Closed,
            reservation_conflicts: bead_conflicts.clone(),
            now,
        },
    );

    let bv_context = triage_context_for_bead(triage, bead_id);
    let ready_minutes = ready_age_minutes(&bead, now);
    let latest_dispatch = Some(DispatchDecisionView {
        attempted_at: ready_in_br.then_some(bead.runtime_updated_at),
        dispatch: DispatchExplanationView::from_eligibility(&eligibility),
        score: bv_context.as_ref().map(|context| context.score),
        score_breakdown: inspect_score_breakdown(
            &bead,
            &dependency_snapshot,
            &bead_conflicts,
            config,
            bv_context.as_ref(),
            ready_minutes,
        ),
        why: inspect_dispatch_why(
            &bead,
            ready_in_br,
            &dependency_snapshot,
            &bead_conflicts,
            bv_context.as_ref(),
        ),
        reservation_conflicts: bead_conflicts
            .iter()
            .map(ReservationConflictView::from_conflict)
            .collect(),
        ready_minutes,
        bv_score: bv_context.as_ref().map(|context| context.score),
    });
    let runs = db.list_task_runs_for_bead(bead_id)?;
    let latest_run = runs.first();
    let latest_session = match latest_run {
        Some(run) => db.latest_session_for_run(&run.id)?.map(|session| {
            let prompt_manifest = session
                .prompt_manifest_path
                .as_deref()
                .and_then(|path| load_prompt_manifest(workspace_root, path));
            SessionSummaryView::from_parts(session, None, prompt_manifest)
        }),
        None => None,
    };

    let selected_playbook_bullets = latest_session
        .as_ref()
        .and_then(|session| session.prompt_provenance.as_ref())
        .map(|prompt| {
            prompt
                .sections
                .iter()
                .filter(|section| section.kind == "playbook" && section.included)
                .flat_map(|section| section.bullet_ids.iter())
                .filter_map(|bullet_id| db.get_playbook_bullet(&BulletId::new(bullet_id.clone())).ok())
                .flatten()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let latest_checkpoint = match (latest_run, db.latest_checkpoint_for_bead(bead_id)?) {
        (Some(run), Some(checkpoint))
            if run
                .last_checkpoint_id
                .as_ref()
                .is_some_and(|checkpoint_id| checkpoint_id == &checkpoint.id) =>
        {
            Some(CheckpointSummaryView::from(checkpoint))
        }
        _ => None,
    };
    let latest_handoff = db.handoff_for_bead(bead_id)?;
    let persisted_recovery_capsule = db.latest_recovery_capsule_for_bead(bead_id)?;
    let latest_recovery_capsule = recovery_capsule_for_inspect(
        &bead,
        latest_run,
        latest_checkpoint.as_ref(),
        latest_session
            .as_ref()
            .and_then(|session| session.prompt_provenance.as_ref()),
        latest_handoff.as_ref(),
        persisted_recovery_capsule.as_ref(),
    );
    let mirror_actions = db
        .list_event_logs_for_bead(bead_id)?
        .into_iter()
        .filter_map(|event| MirrorActionView::from_event(&event))
        .collect();
    let mirror_pending = latest_mirror_pending_for_bead(bead_id, db)?;

    let historical_dispatch_decisions = db.list_dispatch_decisions_for_bead(bead_id, 10).unwrap_or_default();
    let prompt_materializations = db.list_prompt_materializations_for_bead(bead_id).unwrap_or_default();

    let retrieval_bundle = latest_session
        .as_ref()
        .and_then(|session| session.prompt_provenance.as_ref())
        .and_then(retrieval_bundle_from_prompt_provenance);

    Ok(Some(InspectSnapshot {
        bead,
        dependencies: dependency_snapshot
            .blocked_by
            .iter()
            .map(|dependency_id| dependency_edge_view(br, dependency_id, &bead_index))
            .collect(),
        dependents: dependency_snapshot
            .blocks
            .iter()
            .map(|dependent_id| dependency_edge_view(br, dependent_id, &bead_index))
            .collect(),
        latest_dispatch,
        historical_dispatch_decisions,
        prompt_materializations,
        runs,
        latest_session,
        latest_checkpoint,
        latest_recovery_capsule,
        latest_handoff,
        mirror_actions,
        retrieval_bundle,
        selected_playbook_bullets,
        mirror_pending,
    }))
}

fn load_prompt_manifest(workspace_root: &str, path: &str) -> Option<PromptManifest> {
    let manifest_path = if std::path::Path::new(path).is_absolute() {
        std::path::PathBuf::from(path)
    } else {
        std::path::Path::new(workspace_root).join(path)
    };
    let contents = fs::read_to_string(manifest_path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn retrieval_bundle_from_prompt_provenance(
    provenance: &PromptProvenanceView,
) -> Option<RetrievalBundle> {
    let snippets: Vec<RelevantSnippet> = provenance
        .sections
        .iter()
        .filter(|section| section.kind == "archive_snippet")
        .enumerate()
        .filter_map(|(idx, section)| {
            let message_id = section.preview_message_id?;
            Some(RelevantSnippet {
                conversation_id: idx as i64 + 1,
                message_id,
                file_path: None,
                snippet: section.preview.clone(),
                score: 0.0,
            })
        })
        .collect();

    if snippets.is_empty() {
        None
    } else {
        let conversations: Vec<i64> = snippets.iter().map(|snippet| snippet.conversation_id).collect();
        Some(RetrievalBundle {
            snippets,
            conversations,
        })
    }
}

fn inspect_score_breakdown(
    bead: &GroveBeadRecord,
    dependency_snapshot: &grove_br::BrDependencySnapshot,
    conflicts: &[grove_types::ReservationConflict],
    config: &GroveConfig,
    bv_context: Option<&crate::status_view::BvScoreContext<'_>>,
    ready_minutes: Option<i64>,
) -> Vec<ScoreComponentView> {
    let mut breakdown = vec![ScoreComponentView {
        label: "priority".to_owned(),
        value: bead.bead.priority.base_score() as f64,
        note: Some(format!("{:?} priority", bead.bead.priority)),
    }];

    if let Some(context) = bv_context {
        breakdown.push(ScoreComponentView {
            label: "bv_triage".to_owned(),
            value: context.score,
            note: Some(context.summary()),
        });
    }

    if !dependency_snapshot.blocks.is_empty() {
        breakdown.push(ScoreComponentView {
            label: "critical_path".to_owned(),
            value: f64::from(config.scheduler.critical_path_bonus),
            note: Some(format!(
                "{} downstream bead(s)",
                dependency_snapshot.blocks.len()
            )),
        });
    }

    if let Some(minutes) = ready_minutes.filter(|minutes| *minutes > 0) {
        let bonus =
            minutes.min(i64::from(i32::MAX)) as i32 * config.scheduler.ready_age_bonus_per_min;
        breakdown.push(ScoreComponentView {
            label: "ready_age".to_owned(),
            value: f64::from(bonus),
            note: Some(format!("ready for {} minute(s)", minutes)),
        });
    }

    if bead.grove_status == GroveBeadStatus::WaitingToRetry {
        breakdown.push(ScoreComponentView {
            label: "retry_penalty".to_owned(),
            value: -f64::from(config.scheduler.retry_penalty),
            note: Some("waiting to retry".to_owned()),
        });
    }

    if !conflicts.is_empty() {
        breakdown.push(ScoreComponentView {
            label: "reservation_conflict_penalty".to_owned(),
            value: -f64::from(config.scheduler.reservation_conflict_penalty),
            note: Some(format!("{} active conflict(s)", conflicts.len())),
        });
    }

    breakdown
}

fn dependency_edge_view<C: BrClient>(
    br: &C,
    bead_id: &BeadId,
    bead_index: &std::collections::HashMap<BeadId, &GroveBeadRecord>,
) -> DependencyEdgeView {
    let record = bead_index.get(bead_id).copied();
    let br_detail = record.is_none().then(|| br.show(bead_id).ok()).flatten();
    DependencyEdgeView {
        bead_id: bead_id.clone(),
        title: record.map(|record| record.bead.title.clone()).or_else(|| {
            br_detail
                .as_ref()
                .map(|detail| detail.summary.title.clone())
        }),
        br_status: record
            .map(|record| record.bead.br_status.clone())
            .or_else(|| {
                br_detail
                    .as_ref()
                    .map(|detail| detail.summary.status.clone())
            }),
        grove_status: record.map(|record| format!("{:?}", record.grove_status)),
    }
}

fn inspect_dispatch_why(
    bead: &GroveBeadRecord,
    ready_in_br: bool,
    dependency_snapshot: &grove_br::BrDependencySnapshot,
    conflicts: &[grove_types::ReservationConflict],
    bv_context: Option<&crate::status_view::BvScoreContext<'_>>,
) -> Vec<String> {
    let mut why = vec![format!("{:?} priority", bead.bead.priority)];
    if let Some(context) = bv_context {
        why.push(format!(
            "bv triage {:.2}: {}",
            context.score,
            context.summary()
        ));
    }
    if ready_in_br {
        why.push("ready in br".to_owned());
    } else if !dependency_snapshot.blocked_by.is_empty() {
        why.push(format!(
            "blocked by {} bead{} in br",
            dependency_snapshot.blocked_by.len(),
            if dependency_snapshot.blocked_by.len() == 1 {
                ""
            } else {
                "s"
            }
        ));
    } else {
        why.push("not ready in br".to_owned());
    }
    if !dependency_snapshot.blocks.is_empty() {
        why.push(format!(
            "{} downstream bead{}",
            dependency_snapshot.blocks.len(),
            if dependency_snapshot.blocks.len() == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if conflicts.is_empty() {
        why.push("no reservation conflicts".to_owned());
    } else {
        why.push(format!("{} reservation conflict(s)", conflicts.len()));
    }
    why
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectSnapshot {
    pub bead: GroveBeadRecord,
    pub dependencies: Vec<DependencyEdgeView>,
    pub dependents: Vec<DependencyEdgeView>,
    pub latest_dispatch: Option<DispatchDecisionView>,
    pub historical_dispatch_decisions: Vec<DispatchDecisionRecord>,
    pub prompt_materializations: Vec<PromptMaterializationRecord>,
    pub runs: Vec<TaskRunRecord>,
    pub latest_session: Option<SessionSummaryView>,
    pub latest_checkpoint: Option<CheckpointSummaryView>,
    pub latest_recovery_capsule: Option<RecoveryCapsuleView>,
    pub latest_handoff: Option<HandoffRecord>,
    pub mirror_actions: Vec<MirrorActionView>,
    pub retrieval_bundle: Option<RetrievalBundle>,
    pub selected_playbook_bullets: Vec<PlaybookBulletRecord>,
    pub mirror_pending: Option<MirrorPendingView>,
}

impl InspectSnapshot {
    #[must_use]
    pub fn into_view(self) -> BeadInspectView {
        let latest_run = self.runs.first().cloned();
        let run_history = self.runs.into_iter().map(RunSummaryView::from).collect();
        let retrieval_summary = self.retrieval_bundle.map(RetrievalSummaryView::from);
        let latest_handoff = self.latest_handoff.map(HandoffSummaryView::from);
        let playbook_bullets = self
            .selected_playbook_bullets
            .into_iter()
            .map(PlaybookBulletView::from)
            .collect();

        BeadInspectView {
            bead: self.bead,
            dependencies: self.dependencies,
            dependents: self.dependents,
            latest_dispatch: self.latest_dispatch,
            historical_dispatch_decisions: self.historical_dispatch_decisions,
            prompt_materializations: self.prompt_materializations,
            latest_run,
            run_history,
            latest_session: self.latest_session,
            latest_checkpoint: self.latest_checkpoint,
            latest_recovery_capsule: self.latest_recovery_capsule,
            latest_handoff,
            mirror_actions: self.mirror_actions,
            retrieval_summary,
            playbook_bullets,
            mirror_pending: self.mirror_pending,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BeadInspectView {
    pub bead: GroveBeadRecord,
    pub dependencies: Vec<DependencyEdgeView>,
    pub dependents: Vec<DependencyEdgeView>,
    pub latest_dispatch: Option<DispatchDecisionView>,
    pub historical_dispatch_decisions: Vec<DispatchDecisionRecord>,
    pub prompt_materializations: Vec<PromptMaterializationRecord>,
    pub latest_run: Option<TaskRunRecord>,
    pub run_history: Vec<RunSummaryView>,
    pub latest_session: Option<SessionSummaryView>,
    pub latest_checkpoint: Option<CheckpointSummaryView>,
    pub latest_recovery_capsule: Option<RecoveryCapsuleView>,
    pub latest_handoff: Option<HandoffSummaryView>,
    pub mirror_actions: Vec<MirrorActionView>,
    pub retrieval_summary: Option<RetrievalSummaryView>,
    pub playbook_bullets: Vec<PlaybookBulletView>,
    pub mirror_pending: Option<MirrorPendingView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencyEdgeView {
    pub bead_id: BeadId,
    pub title: Option<String>,
    pub br_status: Option<String>,
    pub grove_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DispatchDecisionView {
    pub attempted_at: Option<Timestamp>,
    pub dispatch: DispatchExplanationView,
    pub score: Option<f64>,
    pub score_breakdown: Vec<ScoreComponentView>,
    pub why: Vec<String>,
    pub reservation_conflicts: Vec<ReservationConflictView>,
    pub ready_minutes: Option<i64>,
    pub bv_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummaryView {
    pub run_id: RunId,
    pub attempt_no: i32,
    pub status: String,
    pub failure_class: Option<String>,
    pub failure_detail: Option<String>,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub session_count: i32,
    pub checkpoint_count: i32,
    pub last_checkpoint_id: Option<String>,
}

impl From<TaskRunRecord> for RunSummaryView {
    fn from(run: TaskRunRecord) -> Self {
        Self {
            run_id: run.id,
            attempt_no: run.attempt_no,
            status: format!("{:?}", run.status),
            failure_class: run.failure_class.map(|class| format!("{:?}", class)),
            failure_detail: run.failure_detail,
            started_at: run.started_at,
            ended_at: run.ended_at,
            session_count: run.session_count,
            checkpoint_count: run.checkpoint_count,
            last_checkpoint_id: run.last_checkpoint_id.map(|id| id.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummaryView {
    pub session_id: grove_types::SessionId,
    pub run_id: RunId,
    pub ordinal_in_run: i32,
    pub status: String,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub stop_reason: Option<String>,
    pub terminal_class: Option<String>,
    pub exit_code: Option<i32>,
    pub transcript_path: String,
    pub prompt_id: Option<String>,
    pub prompt_manifest_path: Option<String>,
    pub prompt_bytes: i32,
    pub estimated_input_tokens: i32,
    pub estimated_output_tokens: i32,
    pub prompt_provenance: Option<PromptProvenanceView>,
    pub result_summary: Option<String>,
    pub completion_indicators: Option<u32>,
    pub explicit_exit: Option<bool>,
}

impl SessionSummaryView {
    #[must_use]
    pub fn from_parts(
        session: ClaudeSessionRecord,
        outcome: Option<&SessionOutcome>,
        prompt_manifest: Option<PromptManifest>,
    ) -> Self {
        Self {
            session_id: session.id,
            run_id: session.run_id,
            ordinal_in_run: session.ordinal_in_run,
            status: format!("{:?}", session.status),
            started_at: session.started_at,
            ended_at: session.ended_at,
            stop_reason: session.stop_reason.map(|reason| format!("{:?}", reason)),
            terminal_class: outcome.map(|outcome| format!("{:?}", outcome.terminal_class)),
            exit_code: session.exit_code,
            transcript_path: session.transcript_path,
            prompt_id: session.prompt_id.as_ref().map(ToString::to_string),
            prompt_manifest_path: session.prompt_manifest_path,
            prompt_bytes: session.prompt_bytes,
            estimated_input_tokens: session.estimated_input_tokens,
            estimated_output_tokens: session.estimated_output_tokens,
            prompt_provenance: prompt_manifest.map(PromptProvenanceView::from),
            result_summary: outcome.and_then(|outcome| {
                outcome
                    .protocol_events
                    .iter()
                    .find_map(|event| match event {
                        grove_types::ProtocolEvent::Result { summary } => Some(summary.clone()),
                        _ => None,
                    })
            }),
            completion_indicators: outcome.map(|outcome| outcome.analysis.completion_indicators),
            explicit_exit: outcome.and_then(|outcome| {
                outcome
                    .protocol_events
                    .iter()
                    .rev()
                    .find_map(|event| match event {
                        grove_types::ProtocolEvent::Exit { value } => Some(*value),
                        _ => None,
                    })
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptProvenanceView {
    pub contract: String,
    pub estimated_tokens: u32,
    pub prompt_bytes: u32,
    pub trimmed: bool,
    pub retry_delta_summary: Option<String>,
    pub sections: Vec<PromptSectionView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryCapsuleView {
    pub outcome: String,
    pub summary: String,
    pub strongest_evidence: Vec<String>,
    pub likely_root_causes: Vec<String>,
    pub risky_paths: Vec<String>,
    pub do_not_repeat: Vec<String>,
    pub next_attempt_contract: Option<String>,
    pub retry_delta_summary: Option<String>,
    pub checkpoint_progress: Option<String>,
    pub checkpoint_next_step: Option<String>,
    pub artifacts: Vec<String>,
}

impl From<RecoveryCapsule> for RecoveryCapsuleView {
    fn from(capsule: RecoveryCapsule) -> Self {
        Self {
            outcome: match capsule.outcome {
                RecoveryCapsuleOutcome::Failed => "failed",
                RecoveryCapsuleOutcome::Interrupted => "interrupted",
                RecoveryCapsuleOutcome::Checkpointed => "checkpointed",
            }
            .to_owned(),
            summary: capsule.summary,
            strongest_evidence: capsule.strongest_evidence,
            likely_root_causes: capsule.likely_root_causes,
            risky_paths: capsule.risky_paths,
            do_not_repeat: capsule.do_not_repeat,
            next_attempt_contract: capsule.next_attempt_contract,
            retry_delta_summary: capsule.retry_delta_summary,
            checkpoint_progress: capsule.checkpoint_progress,
            checkpoint_next_step: capsule.checkpoint_next_step,
            artifacts: capsule.artifacts,
        }
    }
}

impl From<PromptManifest> for PromptProvenanceView {
    fn from(manifest: PromptManifest) -> Self {
        Self {
            contract: manifest.contract.as_str().to_owned(),
            estimated_tokens: manifest.estimated_tokens,
            prompt_bytes: manifest.prompt_bytes,
            trimmed: manifest.trimmed,
            retry_delta_summary: manifest.retry_delta_summary,
            sections: manifest
                .sections
                .into_iter()
                .map(PromptSectionView::from)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptSectionView {
    pub ordinal: u32,
    pub kind: String,
    pub heading: String,
    pub included: bool,
    pub estimated_tokens: u32,
    pub trim_reason: Option<String>,
    pub source_ids: Vec<String>,
    pub bullet_ids: Vec<String>,
    pub checkpoint_id: Option<String>,
    pub handoff_run_id: Option<String>,
    pub preview_message_id: Option<i64>,
    pub preview: String,
}

impl From<grove_types::PromptManifestSection> for PromptSectionView {
    fn from(section: grove_types::PromptManifestSection) -> Self {
        let preview_message_id = section
            .provenance
            .archive_message_id
            .as_deref()
            .and_then(|id| id.parse::<i64>().ok());
        Self {
            ordinal: section.ordinal,
            kind: section.kind.as_str().to_owned(),
            heading: section.heading,
            included: section.included,
            estimated_tokens: section.estimated_tokens,
            trim_reason: section.trim_reason.map(|reason| reason.as_str().to_owned()),
            source_ids: section
                .provenance
                .source_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
            bullet_ids: section
                .provenance
                .bullet_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
            checkpoint_id: section.provenance.checkpoint_id.map(|id| id.to_string()),
            handoff_run_id: section.provenance.handoff_run_id.map(|id| id.to_string()),
            preview_message_id,
            preview: section.preview,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckpointSummaryView {
    pub checkpoint_id: String,
    pub run_id: RunId,
    pub session_id: grove_types::SessionId,
    pub progress: String,
    pub next_step: String,
    pub saved_at: Timestamp,
    pub resume_generation: u32,
    pub open_questions: Vec<String>,
    pub claimed_paths: Vec<String>,
    pub confidence: Option<f32>,
}

impl From<CheckpointRecord> for CheckpointSummaryView {
    fn from(checkpoint: CheckpointRecord) -> Self {
        let payload =
            serde_json::from_value::<grove_types::CheckpointPayload>(checkpoint.payload.clone())
                .ok();
        Self {
            checkpoint_id: checkpoint.id.to_string(),
            run_id: checkpoint.run_id,
            session_id: checkpoint.session_id,
            progress: checkpoint.progress,
            next_step: checkpoint.next_step,
            saved_at: checkpoint.saved_at,
            resume_generation: checkpoint.resume_generation,
            open_questions: payload
                .as_ref()
                .map(|payload| payload.open_questions.clone())
                .unwrap_or_default(),
            claimed_paths: payload
                .as_ref()
                .map(|payload| payload.claimed_paths.clone())
                .unwrap_or_default(),
            confidence: payload.and_then(|payload| payload.confidence),
        }
    }
}

fn recovery_capsule_for_inspect(
    bead: &GroveBeadRecord,
    latest_run: Option<&TaskRunRecord>,
    latest_checkpoint: Option<&CheckpointSummaryView>,
    prompt_provenance: Option<&PromptProvenanceView>,
    latest_handoff: Option<&HandoffRecord>,
    persisted_capsule: Option<&RecoveryCapsuleEvent>,
) -> Option<RecoveryCapsuleView> {
    if let Some(event) = persisted_capsule {
        return Some(RecoveryCapsuleView::from(event.capsule.clone()));
    }

    let run = latest_run?;
    let outcome = match bead.grove_status {
        GroveBeadStatus::Checkpointed => RecoveryCapsuleOutcome::Checkpointed,
        GroveBeadStatus::Failed | GroveBeadStatus::WaitingToRetry
            if run.failure_class == Some(grove_types::FailureClass::Interrupted) =>
        {
            RecoveryCapsuleOutcome::Interrupted
        }
        GroveBeadStatus::Failed | GroveBeadStatus::WaitingToRetry => RecoveryCapsuleOutcome::Failed,
        _ => return None,
    };

    RecoveryCapsule::from_parts(
        outcome,
        run.failure_class,
        run.failure_detail.as_deref(),
        latest_checkpoint.map(|checkpoint| checkpoint.progress.as_str()),
        latest_checkpoint.map(|checkpoint| checkpoint.next_step.as_str()),
        prompt_provenance.map(|prompt| prompt.contract.as_str()),
        prompt_provenance.and_then(|prompt| prompt.retry_delta_summary.as_deref()),
        latest_handoff
            .map(|handoff| handoff.artifacts.as_slice())
            .unwrap_or(&[]),
    )
    .map(RecoveryCapsuleView::from)
}

#[derive(Debug, Clone, Serialize)]
pub struct HandoffSummaryView {
    pub run_id: RunId,
    pub summary: String,
    pub artifacts: Vec<String>,
    pub lessons: Vec<String>,
    pub decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub completed_at: Timestamp,
}

impl From<HandoffRecord> for HandoffSummaryView {
    fn from(handoff: HandoffRecord) -> Self {
        Self {
            run_id: handoff.run_id,
            summary: handoff.summary,
            artifacts: handoff.artifacts,
            lessons: handoff.lessons,
            decisions: handoff.decisions,
            warnings: handoff.warnings,
            completed_at: handoff.completed_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MirrorActionView {
    pub event_id: i64,
    pub action: String,
    pub succeeded: Option<bool>,
    pub detail: Option<String>,
    pub created_at: Timestamp,
}

impl MirrorActionView {
    #[must_use]
    pub fn from_event(event: &EventLogRecord) -> Option<Self> {
        let action = match event.kind {
            grove_types::EventKind::BrMirrorRequested => "requested",
            grove_types::EventKind::BrMirrorSucceeded => "succeeded",
            grove_types::EventKind::BrMirrorFailed => "failed",
            _ => return None,
        };

        Some(Self {
            event_id: event.id,
            action: action.to_owned(),
            succeeded: match event.kind {
                grove_types::EventKind::BrMirrorRequested => None,
                grove_types::EventKind::BrMirrorSucceeded => Some(true),
                grove_types::EventKind::BrMirrorFailed => Some(false),
                _ => None,
            },
            detail: (!event.payload.is_null()).then(|| event.payload.to_string()),
            created_at: event.created_at,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RetrievalSummaryView {
    pub conversation_ids: Vec<i64>,
    pub snippet_count: usize,
    pub top_snippets: Vec<RelevantSnippetView>,
}

impl From<RetrievalBundle> for RetrievalSummaryView {
    fn from(bundle: RetrievalBundle) -> Self {
        let snippet_count = bundle.snippets.len();
        let top_snippets = bundle
            .snippets
            .into_iter()
            .take(3)
            .map(RelevantSnippetView::from)
            .collect();

        Self {
            conversation_ids: bundle.conversations,
            snippet_count,
            top_snippets,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RelevantSnippetView {
    pub conversation_id: i64,
    pub message_id: i64,
    pub file_path: Option<String>,
    pub snippet: String,
    pub score: f32,
}

impl From<RelevantSnippet> for RelevantSnippetView {
    fn from(snippet: RelevantSnippet) -> Self {
        Self {
            conversation_id: snippet.conversation_id,
            message_id: snippet.message_id,
            file_path: snippet.file_path.map(|path| path.to_string()),
            snippet: snippet.snippet,
            score: snippet.score,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaybookBulletView {
    pub bullet_id: String,
    pub category: String,
    pub text: String,
    pub maturity: String,
    pub score: Option<f32>,
    pub tags: Vec<String>,
    pub pinned: bool,
}

impl From<PlaybookBulletRecord> for PlaybookBulletView {
    fn from(bullet: PlaybookBulletRecord) -> Self {
        Self {
            bullet_id: bullet.id.to_string(),
            category: bullet.category,
            text: bullet.text,
            maturity: format!("{:?}", bullet.maturity),
            score: bullet.effective_score,
            tags: bullet.tags,
            pinned: bullet.pinned,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status_view::SuppressionReasonView;
    use camino::Utf8PathBuf;
    use grove_br::{
        BrCapability, BrComment, BrDependencySnapshot, BrError, BrIssueDetail, BrIssueSummary,
        BrVersion,
    };
    use grove_db::{Database, RecoveryCapsuleEvent};
    use grove_types::{
        BeadPriority, BeadRef, EventKind, GroveBeadStatus, IterationAnalysis, MessageRole,
        PromptManifest, ProtocolEvent, RecoveryCapsule, RecoveryCapsuleOutcome, SessionStatus,
        SessionTerminalClass, StopReason,
    };
    use std::{collections::BTreeMap, error::Error, io::Error as IoError};
    use tempfile::tempdir;

    type TestResult<T = ()> = Result<T, Box<dyn Error>>;

    #[test]
    fn recovery_capsule_for_inspect_prefers_persisted_capsule() -> TestResult {
        let persisted = RecoveryCapsuleEvent {
            capsule: RecoveryCapsule::from_parts(
                RecoveryCapsuleOutcome::Interrupted,
                Some(grove_types::FailureClass::Interrupted),
                Some("persisted detail"),
                None,
                Some("resume from persisted state"),
                None,
                None,
                &[],
            )
            .ok_or_else(|| IoError::other("expected recovery capsule"))?,
            source_event_id: 9,
            created_at: parse_ts("2026-03-20T06:50:00Z")?,
        };

        let bead = sample_bead()?;
        let run = TaskRunRecord {
            id: RunId::new("run-persisted"),
            bead_id: bead.bead.id.clone(),
            attempt_no: 1,
            status: grove_types::RunStatus::Failed,
            failure_class: Some(grove_types::FailureClass::Interrupted),
            failure_detail: Some("stale detail".to_owned()),
            started_at: parse_ts("2026-03-16T10:00:00Z")?,
            ended_at: Some(parse_ts("2026-03-16T10:10:00Z")?),
            session_count: 1,
            checkpoint_count: 0,
            last_checkpoint_id: None,
            activity: None,
            last_activity_at: None,
            escalation_tier: Default::default(),
        };

        let view =
            recovery_capsule_for_inspect(&bead, Some(&run), None, None, None, Some(&persisted))
                .ok_or_else(|| IoError::other("expected inspect recovery capsule"))?;
        assert_eq!(view.outcome, "interrupted");
        assert_eq!(
            view.checkpoint_next_step.as_deref(),
            Some("resume from persisted state")
        );
        Ok(())
    }

    #[test]
    fn mirror_action_view_maps_mirror_events_only() -> TestResult {
        let event = EventLogRecord {
            id: 17,
            kind: EventKind::BrMirrorFailed,
            bead_id: Some(BeadId::new("grove-1")),
            run_id: Some(RunId::new("run-1")),
            session_id: None,
            payload: "{\"error\":\"boom\"}".parse()?,
            created_at: parse_ts("2026-03-16T12:00:00Z")?,
            // New observability fields
            correlation_id: None,
            operation: None,
            outcome: None,
            duration_ms: None,
            error: None,
            context_snapshot: None,
        };

        let view = MirrorActionView::from_event(&event).ok_or("expected mirror action")?;

        assert_eq!(view.action, "failed");
        assert_eq!(view.succeeded, Some(false));
        assert!(view.detail.as_deref().unwrap_or_default().contains("boom"));
        Ok(())
    }

    #[test]
    fn session_summary_extracts_protocol_result_and_exit_flag() -> TestResult {
        let session = ClaudeSessionRecord {
            id: grove_types::SessionId::new("ses-1"),
            run_id: RunId::new("run-1"),
            external_session_id: None,
            ordinal_in_run: 1,
            status: SessionStatus::Completed,
            started_at: parse_ts("2026-03-16T10:00:00Z")?,
            ended_at: Some(parse_ts("2026-03-16T10:10:00Z")?),
            prompt_id: Some(grove_types::PromptId::new("prompt-1")),
            prompt_manifest_path: Some(".grove/prompts/prompt-1.json".to_owned()),
            prompt_bytes: 12,
            estimated_input_tokens: 34,
            estimated_output_tokens: 56,
            exit_code: Some(0),
            stop_reason: Some(StopReason::Exit),
            transcript_path: ".grove/transcripts/grove-1/ses-1.jsonl".to_owned(),
        };
        let outcome = SessionOutcome {
            session: session.clone(),
            protocol_events: vec![
                ProtocolEvent::Result {
                    summary: "implemented kernel query".to_owned(),
                },
                ProtocolEvent::Exit { value: true },
            ],
            analysis: IterationAnalysis {
                completion_indicators: 3,
                ..IterationAnalysis::default()
            },
            terminal_class: SessionTerminalClass::Success,
            context_pressure_pct: None,
            context_pressure_level: grove_types::ContextPressureLevel::Ok,
            stdout_tail: Vec::new(),
            stderr_tail: Vec::new(),
        };

        let view = SessionSummaryView::from_parts(session, Some(&outcome), None);

        assert_eq!(
            view.result_summary.as_deref(),
            Some("implemented kernel query")
        );
        assert_eq!(view.explicit_exit, Some(true));
        assert_eq!(view.completion_indicators, Some(3));
        assert_eq!(view.prompt_id.as_deref(), Some("prompt-1"));
        assert_eq!(view.ordinal_in_run, 1);
        assert_eq!(view.prompt_bytes, 12);
        assert_eq!(view.estimated_input_tokens, 34);
        assert_eq!(view.estimated_output_tokens, 56);
        assert_eq!(
            view.prompt_manifest_path.as_deref(),
            Some(".grove/prompts/prompt-1.json")
        );
        assert!(view.prompt_provenance.is_none());
        Ok(())
    }

    #[test]
    fn prompt_provenance_view_maps_manifest() -> TestResult {
        let manifest = PromptManifest {
            prompt_id: grove_types::PromptId::new("prompt-1"),
            bead_id: BeadId::new("grove-1"),
            run_id: RunId::new("run-1"),
            session_id: Some(grove_types::SessionId::new("ses-1")),
            contract: grove_types::ExecutionContract::Implement,
            created_at: parse_ts("2026-03-16T10:00:00Z")?,
            token_budget: Some(120),
            estimated_tokens: 91,
            prompt_bytes: 420,
            trimmed: true,
            retry_delta_summary: Some("changed retry framing".to_owned()),
            retrieval_query: None,
            retrieval_ranking_summary: Vec::new(),
            sections: vec![grove_types::PromptManifestSection {
                ordinal: 1,
                kind: grove_types::PromptSegmentKind::Task,
                heading: "Task".to_owned(),
                included: true,
                estimated_tokens: 20,
                char_count: 80,
                trim_reason: Some(grove_types::PromptTrimReason::VerboseParentHandoff),
                provenance: grove_types::PromptSectionProvenance::default(),
                preview: "[TASK]".to_owned(),
            }],
        };

        let view = PromptProvenanceView::from(manifest);
        assert_eq!(view.contract, "implement");
        assert!(view.trimmed);
        assert_eq!(view.sections.len(), 1);
        assert_eq!(view.sections[0].kind, "task");
        assert_eq!(
            view.sections[0].trim_reason.as_deref(),
            Some("verbose_parent_handoff")
        );
        Ok(())
    }

    #[test]
    fn inspect_snapshot_collects_view_sections() -> TestResult {
        let bead = sample_bead()?;
        let snapshot = InspectSnapshot {
            bead,
            dependencies: vec![DependencyEdgeView {
                bead_id: BeadId::new("grove-parent"),
                title: Some("parent".to_owned()),
                br_status: Some("closed".to_owned()),
                grove_status: Some("Succeeded".to_owned()),
            }],
            dependents: vec![DependencyEdgeView {
                bead_id: BeadId::new("grove-child"),
                title: Some("child".to_owned()),
                br_status: Some("open".to_owned()),
                grove_status: Some("Idle".to_owned()),
            }],
            latest_dispatch: Some(DispatchDecisionView {
                attempted_at: Some(parse_ts("2026-03-16T11:00:00Z")?),
                dispatch: DispatchExplanationView {
                    ready_in_br: true,
                    dispatchable_in_grove: false,
                    local_suppression_reasons: vec![SuppressionReasonView {
                        code: "active_run",
                        summary: "active run already owns this bead".to_owned(),
                        run_id: Some(RunId::new("run-1")),
                        retry_after: None,
                        label: None,
                        issue_type: None,
                        conflict: None,
                    }],
                },
                score: Some(123.0),
                score_breakdown: Vec::new(),
                why: vec!["high priority".to_owned()],
                reservation_conflicts: Vec::new(),
                ready_minutes: Some(5),
                bv_score: Some(0.75),
            }),
            historical_dispatch_decisions: Vec::new(),
            prompt_materializations: Vec::new(),
            runs: vec![TaskRunRecord {
                id: RunId::new("run-1"),
                bead_id: BeadId::new("grove-1"),
                attempt_no: 1,
                status: grove_types::RunStatus::Active,
                failure_class: None,
                failure_detail: None,
                started_at: parse_ts("2026-03-16T10:00:00Z")?,
                ended_at: None,
                session_count: 1,
                checkpoint_count: 0,
                last_checkpoint_id: None,
                // New fields for autonomous patterns
                activity: None,
                last_activity_at: None,
                escalation_tier: Default::default(),
            }],
            latest_session: None,
            latest_checkpoint: None,
            latest_handoff: None,
            mirror_actions: vec![MirrorActionView {
                event_id: 1,
                action: "requested".to_owned(),
                succeeded: None,
                detail: None,
                created_at: parse_ts("2026-03-16T11:01:00Z")?,
            }],
            latest_recovery_capsule: None,
            retrieval_bundle: Some(RetrievalBundle {
                snippets: vec![RelevantSnippet {
                    conversation_id: 1,
                    message_id: 2,
                    file_path: None,
                    snippet: "snippet".to_owned(),
                    score: 0.7,
                }],
                conversations: vec![1],
            }),
            selected_playbook_bullets: vec![PlaybookBulletRecord {
                id: grove_types::BulletId::new("bullet-1"),
                scope: grove_types::BulletScope::Workspace,
                scope_key: None,
                category: "workflow".to_owned(),
                text: "Prefer explicit exit markers".to_owned(),
                bullet_type: grove_types::BulletType::Rule,
                state: grove_types::BulletState::Active,
                maturity: grove_types::BulletMaturity::Established,
                helpful_count: 2,
                harmful_count: 0,
                feedback_events: Vec::new(),
                confidence_decay_half_life_days: 14,
                pinned: false,
                deprecated: false,
                replaced_by: None,
                deprecation_reason: None,
                source_bead_ids: vec![BeadId::new("grove-1")],
                source_run_ids: vec![RunId::new("run-1")],
                tags: vec!["phase:1".to_owned()],
                effective_score: Some(0.9),
                created_at: parse_ts("2026-03-16T09:00:00Z")?,
                updated_at: parse_ts("2026-03-16T09:30:00Z")?,
            }],
            mirror_pending: Some(MirrorPendingView {
                bead_id: BeadId::new("grove-1"),
                run_id: Some(RunId::new("run-1")),
                pending_actions: vec!["close".to_owned()],
                last_attempt_at: None,
                last_error: Some("network hiccup".to_owned()),
            }),
        };

        let view = snapshot.into_view();

        assert_eq!(view.dependencies.len(), 1);
        assert_eq!(view.dependents.len(), 1);
        assert_eq!(view.run_history.len(), 1);
        assert_eq!(view.playbook_bullets.len(), 1);
        assert_eq!(
            view.retrieval_summary
                .as_ref()
                .map(|summary| summary.snippet_count),
            Some(1)
        );
        assert!(view.mirror_pending.is_some());
        Ok(())
    }

    #[test]
    fn load_inspect_snapshot_uses_persisted_runtime_and_dependency_data() -> TestResult {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| IoError::other("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, '[]', '[]', '[]', ?8, ?9)",
            rusqlite::params![
                "grove-child",
                "Child bead",
                "Investigate child",
                1,
                "task",
                "open",
                "[\"phase:1\"]",
                "{}",
                "2026-03-16T10:00:00Z",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, '[]', '[]', '[]', '[]', ?7, ?8)",
            rusqlite::params![
                "grove-parent",
                "Parent bead",
                Option::<String>::None,
                0,
                "task",
                "closed",
                "{}",
                "2026-03-16T09:00:00Z",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, '[]', '[]', '[]', '[]', ?7, ?8)",
            rusqlite::params![
                "grove-grandchild",
                "Grandchild bead",
                Option::<String>::None,
                2,
                "task",
                "open",
                "{}",
                "2026-03-16T09:30:00Z",
            ],
        )?;

        db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, ?6, ?7, ?8)",
            rusqlite::params![
                "run-child",
                "grove-child",
                1,
                "Active",
                "2026-03-16T11:00:00Z",
                1,
                1,
                "chk-child",
            ],
        )?;

        db.connection().execute(
            "INSERT INTO bead_runtime(\
                bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                last_failure_class, last_failure_detail, runtime_updated_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL, ?6)",
            rusqlite::params![
                "grove-child",
                "Checkpointed",
                "[\"crates/grove-kernel/src/inspect_view.rs\"]",
                "{}",
                "run-child",
                "2026-03-16T11:10:00Z",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO bead_runtime(\
                bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                last_failure_class, last_failure_detail, runtime_updated_at\
            ) VALUES (?1, ?2, '[]', '{}', NULL, NULL, NULL, NULL, ?3)",
            rusqlite::params!["grove-parent", "Succeeded", "2026-03-16T10:30:00Z"],
        )?;
        db.connection().execute(
            "INSERT INTO bead_runtime(\
                bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                last_failure_class, last_failure_detail, runtime_updated_at\
            ) VALUES (?1, ?2, '[]', '{}', NULL, NULL, NULL, NULL, ?3)",
            rusqlite::params!["grove-grandchild", "Idle", "2026-03-16T10:40:00Z"],
        )?;

        db.connection().execute(
            "INSERT INTO bead_dependencies(parent_id, child_id, relation_type, synced_at) VALUES (?1, ?2, 'blocks', ?3)",
            rusqlite::params!["grove-parent", "grove-child", "2026-03-16T10:00:00Z"],
        )?;
        db.connection().execute(
            "INSERT INTO bead_dependencies(parent_id, child_id, relation_type, synced_at) VALUES (?1, ?2, 'blocks', ?3)",
            rusqlite::params!["grove-child", "grove-grandchild", "2026-03-16T10:05:00Z"],
        )?;
        db.connection().execute(
            "INSERT INTO claude_sessions(\
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                "ses-child",
                "run-child",
                1,
                "Checkpointed",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:08:00Z",
                "prompt-child",
                ".grove/prompts/prompt-child.json",
                150,
                40,
                60,
                0,
                "Checkpoint",
                ".grove/transcripts/grove-child/ses-child.jsonl",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO checkpoints(\
                id, bead_id, run_id, session_id, progress, next_step, payload_json, saved_at, resume_generation\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "chk-child",
                "grove-child",
                "run-child",
                "ses-child",
                "halfway there",
                "resume inspect loader",
                "{\"progress\":\"halfway there\",\"next_step\":\"resume inspect loader\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[\"crates/grove-kernel/src/inspect_view.rs\"],\"confidence\":null}",
                "2026-03-16T11:09:00Z",
                2,
            ],
        )?;
        db.connection().execute(
            "INSERT INTO handoffs(\
                bead_id, run_id, summary, artifacts_json, lessons_json, decisions_json, warnings_json, completed_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                "grove-child",
                "run-child",
                "inspect work in progress",
                "[\"artifact-1\"]",
                "[\"lesson-1\"]",
                "[\"decision-1\"]",
                "[]",
                "2026-03-16T11:12:00Z",
            ],
        )?;
        let mirror_operation = db.enqueue_mirror_outbox(
            &BeadId::new("grove-child"),
            &RunId::new("run-child"),
            &grove_types::HandoffRecord {
                bead_id: BeadId::new("grove-child"),
                run_id: RunId::new("run-child"),
                summary: "inspect work in progress".to_owned(),
                artifacts: vec!["artifact-1".to_owned()],
                lessons: vec!["lesson-1".to_owned()],
                decisions: vec!["decision-1".to_owned()],
                warnings: Vec::new(),
                completed_at: parse_ts("2026-03-16T11:12:00Z")?,
            },
            false,
        )?;
        db.record_mirror_failure(
            &mirror_operation.id,
            &RunId::new("run-child"),
            "network hiccup",
            None,
        )?;

        let mut br = FakeBrClient::new(vec![bead_summary(
            "grove-child",
            "Child bead",
            BeadPriority::P1,
            "open",
            vec!["grove-parent"],
            vec!["grove-grandchild"],
        )]);
        br.details.insert(
            "grove-parent".to_owned(),
            bead_detail(
                "grove-parent",
                "Parent bead",
                BeadPriority::P0,
                "closed",
                vec![],
                vec!["grove-child"],
            ),
        );
        br.details.insert(
            "grove-grandchild".to_owned(),
            bead_detail(
                "grove-grandchild",
                "Grandchild bead",
                BeadPriority::P2,
                "open",
                vec!["grove-child"],
                vec![],
            ),
        );

        let snapshot = load_inspect_snapshot(
            &db,
            &br,
            &BeadId::new("grove-child"),
            dir.path()
                .to_str()
                .ok_or_else(|| IoError::other("temp path was not valid UTF-8"))?,
            &GroveConfig::default(),
            None,
        )?
        .ok_or_else(|| IoError::other("expected inspect snapshot"))?;

        assert_eq!(snapshot.bead.bead.id.as_str(), "grove-child");
        assert_eq!(snapshot.dependencies.len(), 1);
        assert_eq!(snapshot.dependencies[0].bead_id.as_str(), "grove-parent");
        assert_eq!(
            snapshot.dependencies[0].title.as_deref(),
            Some("Parent bead")
        );
        assert_eq!(snapshot.dependents.len(), 1);
        assert_eq!(snapshot.dependents[0].bead_id.as_str(), "grove-grandchild");
        assert_eq!(snapshot.runs.len(), 1);
        assert_eq!(
            snapshot
                .runs
                .first()
                .and_then(|run| run.last_checkpoint_id.as_ref())
                .map(|id| id.as_str()),
            Some("chk-child")
        );
        assert_eq!(
            snapshot
                .latest_session
                .as_ref()
                .map(|session| session.session_id.as_str()),
            Some("ses-child")
        );
        assert_eq!(
            snapshot.latest_session.as_ref().map(|session| (
                session.ordinal_in_run,
                session.prompt_bytes,
                session.estimated_input_tokens,
                session.estimated_output_tokens
            )),
            Some((1, 150, 40, 60))
        );
        assert_eq!(
            snapshot
                .latest_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.checkpoint_id.as_str()),
            Some("chk-child")
        );
        assert_eq!(
            snapshot
                .latest_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.claimed_paths.clone()),
            Some(vec!["crates/grove-kernel/src/inspect_view.rs".to_owned()])
        );
        assert_eq!(
            snapshot
                .latest_handoff
                .as_ref()
                .map(|handoff| handoff.summary.as_str()),
            Some("inspect work in progress")
        );
        assert_eq!(snapshot.mirror_actions.len(), 2);
        assert_eq!(snapshot.mirror_actions[0].action, "failed");
        assert!(snapshot.latest_dispatch.is_some());
        assert!(snapshot.latest_dispatch.as_ref().is_some_and(|dispatch| {
            dispatch
                .dispatch
                .local_suppression_reasons
                .iter()
                .any(|reason| reason.code == "checkpoint_pending_resume")
        }));
        assert!(snapshot.mirror_pending.is_some());
        assert!(snapshot.retrieval_bundle.is_none());
        assert!(snapshot.selected_playbook_bullets.is_empty());
        Ok(())
    }

    #[test]
    fn load_inspect_snapshot_resolves_relative_prompt_manifest_from_workspace_root() -> TestResult {
        let dir = tempdir()?;
        let workspace_root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf())
            .map_err(|_| IoError::other("temp path was not valid UTF-8"))?;
        fs::create_dir_all(workspace_root.join(".grove/prompts"))?;
        fs::write(
            workspace_root.join(".grove/prompts/prompt-child.json"),
            serde_json::to_string(&PromptManifest {
                prompt_id: grove_types::PromptId::new("prompt-child"),
                bead_id: BeadId::new("grove-child"),
                run_id: RunId::new("run-child"),
                session_id: Some(grove_types::SessionId::new("ses-child")),
                contract: grove_types::ExecutionContract::Implement,
                created_at: parse_ts("2026-03-16T11:00:00Z")?,
                token_budget: Some(200),
                estimated_tokens: 120,
                prompt_bytes: 512,
                trimmed: false,
                retry_delta_summary: None,
                retrieval_query: None,
                retrieval_ranking_summary: Vec::new(),
                sections: vec![
                    grove_types::PromptManifestSection {
                        ordinal: 1,
                        kind: grove_types::PromptSegmentKind::Task,
                        heading: "Task".to_owned(),
                        included: true,
                        estimated_tokens: 20,
                        char_count: 80,
                        trim_reason: None,
                        provenance: grove_types::PromptSectionProvenance::default(),
                        preview: "[TASK] fix inspect".to_owned(),
                    },
                    grove_types::PromptManifestSection {
                        ordinal: 2,
                        kind: grove_types::PromptSegmentKind::Playbook,
                        heading: "Playbook workflow (Maturity: Established)".to_owned(),
                        included: true,
                        estimated_tokens: 12,
                        char_count: 48,
                        trim_reason: None,
                        provenance: grove_types::PromptSectionProvenance {
                            bullet_ids: vec![grove_types::BulletId::new("bullet-keep")],
                            ..Default::default()
                        },
                        preview: "[WORKFLOW] prefer explicit markers".to_owned(),
                    },
                ],
            })?,
        )?;
        let db_path = workspace_root.join("grove.db");
        let mut db = Database::open(&db_path)?;
        db.migrate()?;
        db.insert_playbook_bullet(&PlaybookBulletRecord {
            id: grove_types::BulletId::new("bullet-keep"),
            scope: grove_types::BulletScope::Workspace,
            scope_key: None,
            category: "workflow".to_owned(),
            text: "Prefer explicit markers".to_owned(),
            bullet_type: grove_types::BulletType::Rule,
            state: grove_types::BulletState::Active,
            maturity: grove_types::BulletMaturity::Established,
            helpful_count: 4,
            harmful_count: 0,
            feedback_events: Vec::new(),
            confidence_decay_half_life_days: 30,
            pinned: false,
            deprecated: false,
            replaced_by: None,
            deprecation_reason: None,
            source_bead_ids: vec![BeadId::new("grove-child")],
            source_run_ids: vec![RunId::new("run-child")],
            tags: vec!["phase:6".to_owned()],
            effective_score: Some(2.5),
            created_at: parse_ts("2026-03-16T10:30:00Z")?,
            updated_at: parse_ts("2026-03-16T10:45:00Z")?,
        })?;

        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, '[]', '[]', '[]', ?8, ?9)",
            rusqlite::params![
                "grove-child",
                "Child bead",
                "Investigate child",
                1,
                "task",
                "open",
                "[\"phase:1\"]",
                "{}",
                "2026-03-16T10:00:00Z",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, NULL, ?6, ?7, ?8)",
            rusqlite::params![
                "run-child",
                "grove-child",
                1,
                "Active",
                "2026-03-16T11:00:00Z",
                1,
                0,
                Option::<String>::None,
            ],
        )?;
        db.connection().execute(
            "INSERT INTO bead_runtime(\
                bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                last_failure_class, last_failure_detail, runtime_updated_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL, ?6)",
            rusqlite::params![
                "grove-child",
                "Running",
                "[\"crates/grove-kernel/src/inspect_view.rs\"]",
                "{}",
                "run-child",
                "2026-03-16T11:10:00Z",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO claude_sessions(\
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                "ses-child",
                "run-child",
                1,
                "Running",
                "2026-03-16T11:00:00Z",
                Option::<String>::None,
                "prompt-child",
                ".grove/prompts/prompt-child.json",
                150,
                40,
                60,
                Option::<i32>::None,
                Option::<String>::None,
                ".grove/transcripts/grove-child/ses-child.jsonl",
            ],
        )?;

        let br = FakeBrClient::new(vec![bead_summary(
            "grove-child",
            "Child bead",
            BeadPriority::P1,
            "open",
            vec![],
            vec![],
        )]);

        let snapshot = load_inspect_snapshot(
            &db,
            &br,
            &BeadId::new("grove-child"),
            workspace_root.as_str(),
            &GroveConfig::default(),
            None,
        )?
        .ok_or_else(|| IoError::other("expected inspect snapshot"))?;

        assert_eq!(
            snapshot
                .latest_session
                .as_ref()
                .and_then(|session| session.prompt_provenance.as_ref())
                .map(|prompt| prompt.contract.as_str()),
            Some("implement")
        );
        assert_eq!(
            snapshot
                .latest_session
                .as_ref()
                .and_then(|session| session.prompt_provenance.as_ref())
                .map(|prompt| prompt.sections[0].preview.as_str()),
            Some("[TASK] fix inspect")
        );
        assert_eq!(snapshot.selected_playbook_bullets.len(), 1);
        assert_eq!(snapshot.selected_playbook_bullets[0].id.as_str(), "bullet-keep");
        Ok(())
    }

    #[test]
    fn load_inspect_snapshot_hides_stale_checkpoint_from_older_run() -> TestResult {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| IoError::other("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', '{}', ?6)",
            rusqlite::params![
                "grove-child",
                "Child bead",
                1,
                "task",
                "open",
                "2026-03-16T09:00:00Z",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "run-old",
                "grove-child",
                1,
                "Checkpointed",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:10:00Z",
                1,
                1,
                "chk-old",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO task_runs(\
                id, bead_id, attempt_no, status, failure_class, failure_detail, started_at, ended_at, session_count, checkpoint_count, last_checkpoint_id\
            ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "run-new",
                "grove-child",
                2,
                "Succeeded",
                "2026-03-16T12:00:00Z",
                "2026-03-16T12:10:00Z",
                1,
                0,
                Option::<String>::None,
            ],
        )?;
        db.connection().execute(
            "INSERT INTO bead_runtime(\
                bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                last_failure_class, last_failure_detail, runtime_updated_at\
            ) VALUES (?1, ?2, '[]', '{}', ?3, NULL, NULL, NULL, ?4)",
            rusqlite::params![
                "grove-child",
                "Succeeded",
                "run-new",
                "2026-03-16T12:10:00Z",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO claude_sessions(\
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                "ses-new",
                "run-new",
                1,
                "Completed",
                "2026-03-16T12:00:00Z",
                "2026-03-16T12:10:00Z",
                120,
                30,
                45,
                0,
                "Exit",
                ".grove/transcripts/grove-child/ses-new.jsonl",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO claude_sessions(\
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_id, prompt_manifest_path, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                "ses-old",
                "run-old",
                1,
                "Checkpointed",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:09:00Z",
                100,
                25,
                35,
                0,
                "Checkpoint",
                ".grove/transcripts/grove-child/ses-old.jsonl",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO checkpoints(\
                id, bead_id, run_id, session_id, progress, next_step, payload_json, saved_at, resume_generation\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "chk-old",
                "grove-child",
                "run-old",
                "ses-old",
                "halfway there",
                "resume older run",
                "{\"progress\":\"halfway there\",\"next_step\":\"resume older run\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[\"crates/grove-kernel/src/inspect_view.rs\"],\"confidence\":null}",
                "2026-03-16T11:09:00Z",
                1,
            ],
        )?;

        let br = FakeBrClient::new(vec![bead_summary(
            "grove-child",
            "Child bead",
            BeadPriority::P1,
            "open",
            vec![],
            vec![],
        )]);

        let snapshot = load_inspect_snapshot(
            &db,
            &br,
            &BeadId::new("grove-child"),
            dir.path()
                .to_str()
                .ok_or_else(|| IoError::other("temp path was not valid UTF-8"))?,
            &GroveConfig::default(),
            None,
        )?
        .ok_or_else(|| IoError::other("expected inspect snapshot"))?;

        assert_eq!(snapshot.runs.len(), 2);
        assert_eq!(
            snapshot
                .latest_session
                .as_ref()
                .map(|session| session.run_id.as_str()),
            Some("run-new")
        );
        assert!(snapshot.latest_checkpoint.is_none());
        Ok(())
    }

    #[test]
    fn load_inspect_snapshot_explains_not_ready_beads_from_dependency_state() -> TestResult {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| IoError::other("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, '[]', '[]', '[]', '[]', ?7, ?8)",
            rusqlite::params![
                "grove-blocked",
                "Blocked bead",
                Option::<String>::None,
                0,
                "task",
                "open",
                "{}",
                "2026-03-16T10:00:00Z",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, '[]', '[]', '[]', '[]', ?7, ?8)",
            rusqlite::params![
                "grove-parent",
                "Parent bead",
                Option::<String>::None,
                1,
                "task",
                "open",
                "{}",
                "2026-03-16T09:00:00Z",
            ],
        )?;
        db.connection().execute(
            "INSERT INTO bead_runtime(\
                bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                last_failure_class, last_failure_detail, runtime_updated_at\
            ) VALUES (?1, ?2, '[]', '{}', NULL, NULL, NULL, NULL, ?3)",
            rusqlite::params!["grove-blocked", "Idle", "2026-03-16T10:10:00Z"],
        )?;
        db.connection().execute(
            "INSERT INTO bead_runtime(\
                bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                last_failure_class, last_failure_detail, runtime_updated_at\
            ) VALUES (?1, ?2, '[]', '{}', NULL, NULL, NULL, NULL, ?3)",
            rusqlite::params!["grove-parent", "Ready", "2026-03-16T09:10:00Z"],
        )?;
        db.connection().execute(
            "INSERT INTO bead_dependencies(parent_id, child_id, relation_type, synced_at) VALUES (?1, ?2, 'blocks', ?3)",
            rusqlite::params!["grove-parent", "grove-blocked", "2026-03-16T10:05:00Z"],
        )?;

        let br = FakeBrClient::new(Vec::new());

        let snapshot = load_inspect_snapshot(
            &db,
            &br,
            &BeadId::new("grove-blocked"),
            dir.path()
                .to_str()
                .ok_or_else(|| IoError::other("temp path was not valid UTF-8"))?,
            &GroveConfig::default(),
            None,
        )?
        .ok_or_else(|| IoError::other("expected inspect snapshot"))?;

        let latest_dispatch = snapshot
            .latest_dispatch
            .as_ref()
            .ok_or_else(|| IoError::other("expected dispatch explanation"))?;
        assert!(!latest_dispatch.dispatch.ready_in_br);
        assert!(!latest_dispatch.dispatch.dispatchable_in_grove);
        assert_eq!(latest_dispatch.attempted_at, None);
        assert_eq!(latest_dispatch.why[1], "blocked by 1 bead in br");
        assert_eq!(latest_dispatch.dispatch.summary(), "not ready in br");
        Ok(())
    }

    fn sample_bead() -> TestResult<GroveBeadRecord> {
        let created_at = parse_ts("2026-03-16T09:00:00Z")?;
        let updated_at = parse_ts("2026-03-16T09:30:00Z")?;
        Ok(GroveBeadRecord {
            bead: BeadRef {
                id: BeadId::new("grove-1"),
                title: "kernel status views".to_owned(),
                description: Some("add status/inspect DTOs".to_owned()),
                priority: BeadPriority::P0,
                issue_type: "task".to_owned(),
                br_status: "open".to_owned(),
                assignee: None,
                labels: vec!["phase:1".to_owned()],
                created_at,
                updated_at,
            },
            grove_status: GroveBeadStatus::Ready,
            declared_paths: vec!["crates/grove-kernel/src/*.rs".to_owned()],
            metadata: "{\"source\":\"test\"}".parse()?,
            last_run_id: Some(RunId::new("run-1")),
            retry_after: None,
            last_failure_class: None,
            last_failure_detail: None,
            synced_at: updated_at,
            runtime_updated_at: updated_at,
        })
    }

    fn parse_ts(value: &str) -> TestResult<Timestamp> {
        Ok(value.parse()?)
    }

    fn bead_summary(
        id: &str,
        title: &str,
        priority: BeadPriority,
        status: &str,
        blocked_by: Vec<&str>,
        blocks: Vec<&str>,
    ) -> BrIssueSummary {
        BrIssueSummary {
            id: BeadId::new(id),
            title: title.to_owned(),
            description: None,
            priority,
            issue_type: "task".to_owned(),
            status: status.to_owned(),
            assignee: None,
            labels: Vec::new(),
            created_at: "2026-03-16T09:00:00Z"
                .parse()
                .expect("static timestamp should parse"),
            updated_at: "2026-03-16T09:30:00Z"
                .parse()
                .expect("static timestamp should parse"),
            blocked_by: blocked_by.into_iter().map(BeadId::new).collect(),
            blocks: blocks.into_iter().map(BeadId::new).collect(),
            raw_json: "{}".parse().expect("empty JSON object should parse"),
        }
    }

    fn bead_detail(
        id: &str,
        title: &str,
        priority: BeadPriority,
        status: &str,
        blocked_by: Vec<&str>,
        blocks: Vec<&str>,
    ) -> BrIssueDetail {
        BrIssueDetail {
            summary: bead_summary(id, title, priority, status, blocked_by, blocks),
            closed_at: None,
            close_reason: None,
            comments: Vec::<BrComment>::new(),
            metadata: "{}".parse().expect("empty JSON object should parse"),
        }
    }

    struct FakeBrClient {
        ready: Vec<BrIssueSummary>,
        list_open: Vec<BrIssueSummary>,
        details: BTreeMap<String, BrIssueDetail>,
        dep_snapshots: BTreeMap<String, BrDependencySnapshot>,
    }

    impl FakeBrClient {
        fn new(ready: Vec<BrIssueSummary>) -> Self {
            Self {
                list_open: ready.clone(),
                ready,
                details: BTreeMap::new(),
                dep_snapshots: BTreeMap::new(),
            }
        }
    }

    impl BrClient for FakeBrClient {
        fn ready(&self) -> Result<Vec<BrIssueSummary>, BrError> {
            Ok(self.ready.clone())
        }

        fn list_open(&self) -> Result<Vec<BrIssueSummary>, BrError> {
            Ok(self.list_open.clone())
        }

        fn show(&self, id: &BeadId) -> Result<BrIssueDetail, BrError> {
            self.details
                .get(id.as_str())
                .cloned()
                .ok_or_else(|| BrError::BeadNotFound { id: id.clone() })
        }

        fn dep_list(&self, id: &BeadId) -> Result<BrDependencySnapshot, BrError> {
            self.dep_snapshots
                .get(id.as_str())
                .cloned()
                .ok_or_else(|| BrError::BeadNotFound { id: id.clone() })
        }

        fn capability(&self) -> Result<BrCapability, BrError> {
            Ok(BrCapability {
                available: true,
                version_line: Some("br 0.1.12".to_owned()),
                version: Some(BrVersion {
                    raw: "br 0.1.12".to_owned(),
                    major: Some(0),
                    minor: Some(1),
                    patch: Some(12),
                }),
                beads_dir_exists: true,
            })
        }

        fn close_bead(&self, _id: &BeadId, _resolution: Option<&str>) -> Result<(), BrError> {
            // Fake implementation - always succeeds
            Ok(())
        }

        fn add_comment(&self, _id: &BeadId, _text: &str) -> Result<(), BrError> {
            // Fake implementation - always succeeds
            Ok(())
        }

        fn mirror_handoff(
            &self,
            _id: &BeadId,
            _handoff: &grove_types::HandoffRecord,
            _close_bead: bool,
        ) -> Result<(), BrError> {
            // Fake implementation - always succeeds
            Ok(())
        }
    }

    #[allow(dead_code)]
    fn _unused_roles_for_future_expansion() -> [MessageRole; 4] {
        [
            MessageRole::User,
            MessageRole::Agent,
            MessageRole::Tool,
            MessageRole::System,
        ]
    }
}
