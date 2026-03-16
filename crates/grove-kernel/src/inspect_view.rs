use crate::status_view::{DispatchExplanationView, MirrorPendingView, ReservationConflictView};
use grove_types::{
    BeadId, CheckpointRecord, ClaudeSessionRecord, EventLogRecord, GroveBeadRecord, HandoffRecord,
    PlaybookBulletRecord, RelevantSnippet, RetrievalBundle, RunId, SessionOutcome, TaskRunRecord,
    Timestamp,
};

pub const QUERY_PURPOSE: &str =
    "Operator-facing inspect query models for grove inspect bead diagnostics.";

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
    pub fn from_parts(
        session: ClaudeSessionRecord,
        outcome: Option<&SessionOutcome>,
    ) -> Self {
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
                outcome.protocol_events.iter().find_map(|event| match event {
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
    use grove_types::{
        BeadPriority, BeadRef, EventKind, GroveBeadStatus, IterationAnalysis, MessageRole,
        ProtocolEvent, SessionStatus, SessionTerminalClass, StopReason,
    };
    use std::error::Error;

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

        assert_eq!(view.result_summary.as_deref(), Some("implemented kernel query"));
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
        assert_eq!(view.retrieval_summary.as_ref().map(|summary| summary.snippet_count), Some(1));
        assert!(view.mirror_pending.is_some());
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

    #[allow(dead_code)]
    fn _unused_roles_for_future_expansion() -> [MessageRole; 4] {
        [MessageRole::User, MessageRole::Agent, MessageRole::Tool, MessageRole::System]
    }
}
