#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::status_view::{
    DispatchExplanationView, MirrorPendingView, ReservationConflictView, ScoreComponentView,
    conflicts_for_bead, find_reservation_conflicts, latest_mirror_pending_for_bead,
    ready_age_minutes, triage_context_for_bead,
};
use crate::{DispatchEligibilityContext, evaluate_dispatch_eligibility};
use anyhow::Result;
use chrono::Utc;
use grove_br::BrClient;
use grove_bv::BvTriageOutput;
use grove_config::GroveConfig;
use grove_db::{Database, RecoveryCapsuleEvent};
use grove_types::{
    BeadId, BulletId, CheckpointRecord, ClaudeSessionRecord, DispatchDecisionRecord,
    EventLogRecord, GroveBeadRecord, GroveBeadStatus, HandoffRecord, PlaybookBulletRecord,
    PromptManifest, PromptMaterializationRecord, RecoveryCapsule, RecoveryCapsuleOutcome,
    RelevantSnippet, RetrievalBundle, RunId, RunReport, SessionOutcome, TaskRunRecord, Timestamp,
};
use serde::Serialize;
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
            circuit_state: crate::circuit_state_for_bead(&bead),
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
                .filter_map(|bullet_id| {
                    db.get_playbook_bullet(&BulletId::new(bullet_id.clone()))
                        .ok()
                })
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

    let historical_dispatch_decisions = db
        .list_dispatch_decisions_for_bead(bead_id, 10)
        .unwrap_or_default();
    let prompt_materializations = db
        .list_prompt_materializations_for_bead(bead_id)
        .unwrap_or_default();

    let retrieval_bundle = latest_session
        .as_ref()
        .and_then(|session| session.prompt_provenance.as_ref())
        .and_then(retrieval_bundle_from_prompt_provenance);

    let run_report = latest_run
        .as_ref()
        .and_then(|run| db.generate_run_report(&run.id).ok().flatten());

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
        run_report,
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
        let conversations: Vec<i64> = snippets
            .iter()
            .map(|snippet| snippet.conversation_id)
            .collect();
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
    pub run_report: Option<RunReport>,
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
            run_report: self.run_report,
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
    pub run_report: Option<RunReport>,
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
mod tests;
