use crate::status_view::{
    conflicts_for_bead, find_reservation_conflicts, latest_mirror_pending_for_bead,
    DispatchExplanationView, MirrorPendingView, ReservationConflictView,
};
use crate::{evaluate_dispatch_eligibility, DispatchEligibilityContext};
use anyhow::Result;
use chrono::Utc;
use grove_br::BrClient;
use grove_config::GroveConfig;
use grove_db::Database;
use grove_types::{
    BeadId, CheckpointRecord, ClaudeSessionRecord, EventLogRecord, GroveBeadRecord, HandoffRecord,
    PlaybookBulletRecord, RelevantSnippet, RetrievalBundle, RunId, SessionOutcome, TaskRunRecord,
    Timestamp,
};

pub const QUERY_PURPOSE: &str =
    "Operator-facing inspect query models for grove inspect bead diagnostics.";

pub fn load_inspect_snapshot<C: BrClient>(
    db: &Database,
    br: &C,
    bead_id: &BeadId,
    _config: &GroveConfig,
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
    let eligibility = evaluate_dispatch_eligibility(
        &bead,
        &DispatchEligibilityContext {
            ready_in_br,
            circuit_state: grove_types::CircuitState::Closed,
            reservation_conflicts: bead_conflicts.clone(),
            now: Utc::now(),
        },
    );

    let latest_dispatch = Some(DispatchDecisionView {
        attempted_at: ready_in_br.then_some(bead.runtime_updated_at),
        dispatch: DispatchExplanationView::from_eligibility(&eligibility),
        score: None,
        why: inspect_dispatch_why(&bead, ready_in_br, &dependency_snapshot, &bead_conflicts),
        reservation_conflicts: bead_conflicts
            .iter()
            .map(ReservationConflictView::from_conflict)
            .collect(),
    });
    let runs = db.list_task_runs_for_bead(bead_id)?;
    let latest_session = match runs.first() {
        Some(run) => db
            .latest_session_for_run(&run.id)?
            .map(|session| SessionSummaryView::from_parts(session, None)),
        None => None,
    };
    let latest_checkpoint = db
        .latest_checkpoint_for_bead(bead_id)?
        .map(CheckpointSummaryView::from);
    let latest_handoff = db.handoff_for_bead(bead_id)?;
    let mirror_actions = db
        .list_event_logs_for_bead(bead_id)?
        .into_iter()
        .filter_map(|event| MirrorActionView::from_event(&event))
        .collect();
    let mirror_pending = latest_mirror_pending_for_bead(bead_id, db)?;

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
        runs,
        latest_session,
        latest_checkpoint,
        latest_handoff,
        mirror_actions,
        retrieval_bundle: None,
        selected_playbook_bullets: Vec::new(),
        mirror_pending,
    }))
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
) -> Vec<String> {
    let mut why = vec![format!("{:?} priority", bead.bead.priority)];
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

#[derive(Debug, Clone)]
pub struct InspectSnapshot {
    pub bead: GroveBeadRecord,
    pub dependencies: Vec<DependencyEdgeView>,
    pub dependents: Vec<DependencyEdgeView>,
    pub latest_dispatch: Option<DispatchDecisionView>,
    pub runs: Vec<TaskRunRecord>,
    pub latest_session: Option<SessionSummaryView>,
    pub latest_checkpoint: Option<CheckpointSummaryView>,
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
            latest_run,
            run_history,
            latest_session: self.latest_session,
            latest_checkpoint: self.latest_checkpoint,
            latest_handoff,
            mirror_actions: self.mirror_actions,
            retrieval_summary,
            playbook_bullets,
            mirror_pending: self.mirror_pending,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BeadInspectView {
    pub bead: GroveBeadRecord,
    pub dependencies: Vec<DependencyEdgeView>,
    pub dependents: Vec<DependencyEdgeView>,
    pub latest_dispatch: Option<DispatchDecisionView>,
    pub latest_run: Option<TaskRunRecord>,
    pub run_history: Vec<RunSummaryView>,
    pub latest_session: Option<SessionSummaryView>,
    pub latest_checkpoint: Option<CheckpointSummaryView>,
    pub latest_handoff: Option<HandoffSummaryView>,
    pub mirror_actions: Vec<MirrorActionView>,
    pub retrieval_summary: Option<RetrievalSummaryView>,
    pub playbook_bullets: Vec<PlaybookBulletView>,
    pub mirror_pending: Option<MirrorPendingView>,
}

#[derive(Debug, Clone)]
pub struct DependencyEdgeView {
    pub bead_id: BeadId,
    pub title: Option<String>,
    pub br_status: Option<String>,
    pub grove_status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DispatchDecisionView {
    pub attempted_at: Option<Timestamp>,
    pub dispatch: DispatchExplanationView,
    pub score: Option<f64>,
    pub why: Vec<String>,
    pub reservation_conflicts: Vec<ReservationConflictView>,
}

#[derive(Debug, Clone)]
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
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionSummaryView {
    pub session_id: grove_types::SessionId,
    pub run_id: RunId,
    pub status: String,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub stop_reason: Option<String>,
    pub terminal_class: Option<String>,
    pub exit_code: Option<i32>,
    pub transcript_path: String,
    pub result_summary: Option<String>,
    pub completion_indicators: Option<u32>,
    pub explicit_exit: Option<bool>,
}

impl SessionSummaryView {
    #[must_use]
    pub fn from_parts(session: ClaudeSessionRecord, outcome: Option<&SessionOutcome>) -> Self {
        Self {
            session_id: session.id,
            run_id: session.run_id,
            status: format!("{:?}", session.status),
            started_at: session.started_at,
            ended_at: session.ended_at,
            stop_reason: session.stop_reason.map(|reason| format!("{:?}", reason)),
            terminal_class: outcome.map(|outcome| format!("{:?}", outcome.terminal_class)),
            exit_code: session.exit_code,
            transcript_path: session.transcript_path,
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

#[derive(Debug, Clone)]
pub struct CheckpointSummaryView {
    pub checkpoint_id: String,
    pub run_id: RunId,
    pub session_id: grove_types::SessionId,
    pub progress: String,
    pub next_step: String,
    pub saved_at: Timestamp,
    pub resume_generation: u32,
}

impl From<CheckpointRecord> for CheckpointSummaryView {
    fn from(checkpoint: CheckpointRecord) -> Self {
        Self {
            checkpoint_id: checkpoint.id.to_string(),
            run_id: checkpoint.run_id,
            session_id: checkpoint.session_id,
            progress: checkpoint.progress,
            next_step: checkpoint.next_step,
            saved_at: checkpoint.saved_at,
            resume_generation: checkpoint.resume_generation,
        }
    }
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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
    use grove_db::Database;
    use grove_types::{
        BeadPriority, BeadRef, EventKind, GroveBeadStatus, IterationAnalysis, MessageRole,
        ProtocolEvent, SessionStatus, SessionTerminalClass, StopReason,
    };
    use std::{collections::BTreeMap, error::Error, io::Error as IoError};
    use tempfile::tempdir;

    type TestResult<T = ()> = Result<T, Box<dyn Error>>;

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
            stdout_tail: Vec::new(),
            stderr_tail: Vec::new(),
        };

        let view = SessionSummaryView::from_parts(session, Some(&outcome));

        assert_eq!(
            view.result_summary.as_deref(),
            Some("implemented kernel query")
        );
        assert_eq!(view.explicit_exit, Some(true));
        assert_eq!(view.completion_indicators, Some(3));
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
                why: vec!["high priority".to_owned()],
                reservation_conflicts: Vec::new(),
            }),
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
                id, run_id, external_session_id, ordinal_in_run, status, started_at, ended_at, prompt_bytes, estimated_input_tokens, estimated_output_tokens, exit_code, stop_reason, transcript_path\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                "ses-child",
                "run-child",
                1,
                "Checkpointed",
                "2026-03-16T11:00:00Z",
                "2026-03-16T11:08:00Z",
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
                "{\"claimed_paths\":[\"crates/grove-kernel/src/inspect_view.rs\"]}",
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
        db.connection().execute(
            "INSERT INTO event_log(kind, bead_id, run_id, session_id, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "BrMirrorFailed",
                "grove-child",
                "run-child",
                "ses-child",
                "{\"error\":\"network hiccup\"}",
                "2026-03-16T11:13:00Z",
            ],
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
            &GroveConfig::default(),
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
                .latest_session
                .as_ref()
                .map(|session| session.session_id.as_str()),
            Some("ses-child")
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
                .latest_handoff
                .as_ref()
                .map(|handoff| handoff.summary.as_str()),
            Some("inspect work in progress")
        );
        assert_eq!(snapshot.mirror_actions.len(), 1);
        assert_eq!(snapshot.mirror_actions[0].action, "failed");
        assert!(snapshot.latest_dispatch.is_some());
        assert!(snapshot
            .latest_dispatch
            .as_ref()
            .is_some_and(|dispatch| dispatch
                .dispatch
                .local_suppression_reasons
                .iter()
                .any(|reason| reason.code == "checkpoint_pending_resume")));
        assert!(snapshot.mirror_pending.is_some());
        assert!(snapshot.retrieval_bundle.is_none());
        assert!(snapshot.selected_playbook_bullets.is_empty());
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
            &GroveConfig::default(),
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
