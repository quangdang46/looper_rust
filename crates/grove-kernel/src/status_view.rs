use crate::{DispatchEligibility, LocalSuppressionReason};
use grove_types::{
    BeadId, BeadPriority, FailureClass, GroveBeadRecord, GroveBeadStatus, ReservationConflict,
    RunId, SessionId, Timestamp,
};
use std::collections::BTreeMap;

pub const QUERY_PURPOSE: &str =
    "Operator-facing status query models for grove status and dispatch explainability.";

#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub workspace_root: String,
    pub leader: Option<LeaderLeaseView>,
    pub beads: Vec<GroveBeadRecord>,
    pub running_beads: Vec<RunningBeadView>,
    pub ready_queue: Vec<ReadyQueueEntry>,
    pub checkpointed_beads: Vec<CheckpointedBeadView>,
    pub failed_beads: Vec<FailedBeadView>,
    pub reservation_conflicts: Vec<ReservationConflictView>,
    pub mirror_pending: Vec<MirrorPendingView>,
}

impl StatusSnapshot {
    #[must_use]
    pub fn into_view(self) -> WorkspaceStatusView {
        WorkspaceStatusView {
            workspace_root: self.workspace_root,
            leader: self.leader,
            bead_status_counts: count_beads_statuses(&self.beads),
            grove_status_counts: count_grove_statuses(&self.beads),
            running_beads: self.running_beads,
            ready_queue: self.ready_queue,
            checkpointed_beads: self.checkpointed_beads,
            failed_beads: self.failed_beads,
            reservation_conflicts: self.reservation_conflicts,
            mirror_pending: self.mirror_pending,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceStatusView {
    pub workspace_root: String,
    pub leader: Option<LeaderLeaseView>,
    pub bead_status_counts: Vec<StatusCount>,
    pub grove_status_counts: Vec<StatusCount>,
    pub running_beads: Vec<RunningBeadView>,
    pub ready_queue: Vec<ReadyQueueEntry>,
    pub checkpointed_beads: Vec<CheckpointedBeadView>,
    pub failed_beads: Vec<FailedBeadView>,
    pub reservation_conflicts: Vec<ReservationConflictView>,
    pub mirror_pending: Vec<MirrorPendingView>,
}

#[derive(Debug, Clone)]
pub struct StatusCount {
    pub status: String,
    pub count: usize,
}

#[derive(Debug, Clone)]
pub struct LeaderLeaseView {
    pub owner_label: String,
    pub acquired_at: Option<Timestamp>,
    pub heartbeat_at: Option<Timestamp>,
    pub expires_at: Option<Timestamp>,
}

#[derive(Debug, Clone)]
pub struct RunningBeadView {
    pub bead_id: BeadId,
    pub title: String,
    pub priority: BeadPriority,
    pub br_status: String,
    pub grove_status: GroveBeadStatus,
    pub run_id: Option<RunId>,
    pub session_id: Option<SessionId>,
    pub started_at: Option<Timestamp>,
    pub context_pressure_pct: Option<f32>,
    pub last_progress: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReadyQueueEntry {
    pub bead_id: BeadId,
    pub title: String,
    pub priority: BeadPriority,
    pub score: Option<f64>,
    pub score_breakdown: Vec<ScoreComponentView>,
    pub why: Vec<String>,
    pub dispatch: DispatchExplanationView,
    pub mirror_pending: bool,
}

#[derive(Debug, Clone)]
pub struct ScoreComponentView {
    pub label: String,
    pub value: f64,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CheckpointedBeadView {
    pub bead_id: BeadId,
    pub title: String,
    pub run_id: Option<RunId>,
    pub checkpoint_id: Option<String>,
    pub progress: Option<String>,
    pub next_step: Option<String>,
    pub claimed_paths: Vec<String>,
    pub saved_at: Option<Timestamp>,
}

#[derive(Debug, Clone)]
pub struct FailedBeadView {
    pub bead_id: BeadId,
    pub title: String,
    pub priority: BeadPriority,
    pub run_id: Option<RunId>,
    pub failure_class: Option<FailureClass>,
    pub failure_detail: Option<String>,
    pub retry_after: Option<Timestamp>,
    pub dispatch: Option<DispatchExplanationView>,
    pub recovery_hint: Option<String>,
    pub mirror_pending: bool,
}

#[derive(Debug, Clone)]
pub struct MirrorPendingView {
    pub bead_id: BeadId,
    pub run_id: Option<RunId>,
    pub pending_actions: Vec<String>,
    pub last_attempt_at: Option<Timestamp>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DispatchExplanationView {
    pub ready_in_br: bool,
    pub dispatchable_in_grove: bool,
    pub local_suppression_reasons: Vec<SuppressionReasonView>,
}

impl DispatchExplanationView {
    #[must_use]
    pub fn from_eligibility(eligibility: &DispatchEligibility) -> Self {
        Self {
            ready_in_br: eligibility.ready_in_br,
            dispatchable_in_grove: eligibility.dispatchable_in_grove,
            local_suppression_reasons: eligibility
                .local_suppression_reasons
                .iter()
                .map(SuppressionReasonView::from_reason)
                .collect(),
        }
    }

    #[must_use]
    pub fn summary(&self) -> String {
        if self.dispatchable_in_grove {
            return "dispatchable".to_owned();
        }

        if !self.ready_in_br {
            return "not ready in br".to_owned();
        }

        if self.local_suppression_reasons.is_empty() {
            return "not dispatchable".to_owned();
        }

        self.local_suppression_reasons
            .iter()
            .map(|reason| reason.summary.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone)]
pub struct SuppressionReasonView {
    pub code: &'static str,
    pub summary: String,
    pub run_id: Option<RunId>,
    pub retry_after: Option<Timestamp>,
    pub label: Option<String>,
    pub issue_type: Option<String>,
    pub conflict: Option<ReservationConflictView>,
}

impl SuppressionReasonView {
    #[must_use]
    pub fn from_reason(reason: &LocalSuppressionReason) -> Self {
        match reason {
            LocalSuppressionReason::SuppressedByLabel { label } => Self {
                code: reason.code(),
                summary: format!("suppressed by label {label}"),
                run_id: None,
                retry_after: None,
                label: Some(label.clone()),
                issue_type: None,
                conflict: None,
            },
            LocalSuppressionReason::NonExecutableIssueType { issue_type } => Self {
                code: reason.code(),
                summary: format!("non-executable issue type {issue_type}"),
                run_id: None,
                retry_after: None,
                label: None,
                issue_type: Some(issue_type.clone()),
                conflict: None,
            },
            LocalSuppressionReason::ActiveRun { run_id } => Self {
                code: reason.code(),
                summary: "active run already owns this bead".to_owned(),
                run_id: run_id.clone(),
                retry_after: None,
                label: None,
                issue_type: None,
                conflict: None,
            },
            LocalSuppressionReason::CheckpointPendingResume { run_id } => Self {
                code: reason.code(),
                summary: "checkpoint pending resume".to_owned(),
                run_id: run_id.clone(),
                retry_after: None,
                label: None,
                issue_type: None,
                conflict: None,
            },
            LocalSuppressionReason::RetryBackoffPending { retry_after } => Self {
                code: reason.code(),
                summary: "retry backoff still pending".to_owned(),
                run_id: None,
                retry_after: retry_after.clone(),
                label: None,
                issue_type: None,
                conflict: None,
            },
            LocalSuppressionReason::CircuitOpen => Self {
                code: reason.code(),
                summary: "circuit breaker is open".to_owned(),
                run_id: None,
                retry_after: None,
                label: None,
                issue_type: None,
                conflict: None,
            },
            LocalSuppressionReason::ReservationConflict { conflict } => Self {
                code: reason.code(),
                summary: format!(
                    "reservation conflict with {} on {}",
                    conflict.conflicting_bead, conflict.held_pattern
                ),
                run_id: conflict.conflicting_run_id.clone(),
                retry_after: None,
                label: None,
                issue_type: None,
                conflict: Some(ReservationConflictView::from_conflict(conflict)),
            },
            LocalSuppressionReason::AlreadySucceeded => Self {
                code: reason.code(),
                summary: "already succeeded locally".to_owned(),
                run_id: None,
                retry_after: None,
                label: None,
                issue_type: None,
                conflict: None,
            },
            LocalSuppressionReason::FailedAwaitingManualRetry => Self {
                code: reason.code(),
                summary: "failed and awaiting manual retry".to_owned(),
                run_id: None,
                retry_after: None,
                label: None,
                issue_type: None,
                conflict: None,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReservationConflictView {
    pub requested_by_bead: BeadId,
    pub conflicting_bead: BeadId,
    pub requested_pattern: String,
    pub held_pattern: String,
    pub conflicting_run_id: Option<RunId>,
}

impl ReservationConflictView {
    #[must_use]
    pub fn from_conflict(conflict: &ReservationConflict) -> Self {
        Self {
            requested_by_bead: conflict.requested_by_bead.clone(),
            conflicting_bead: conflict.conflicting_bead.clone(),
            requested_pattern: conflict.requested_pattern.clone(),
            held_pattern: conflict.held_pattern.clone(),
            conflicting_run_id: conflict.conflicting_run_id.clone(),
        }
    }
}

#[must_use]
pub fn count_beads_statuses(beads: &[GroveBeadRecord]) -> Vec<StatusCount> {
    count_strings(beads.iter().map(|bead| bead.bead.br_status.as_str()))
}

#[must_use]
pub fn count_grove_statuses(beads: &[GroveBeadRecord]) -> Vec<StatusCount> {
    count_strings(beads.iter().map(|bead| grove_status_label(bead.grove_status)))
}

fn count_strings<'a>(values: impl Iterator<Item = &'a str>) -> Vec<StatusCount> {
    let mut counts = BTreeMap::<String, usize>::new();
    for value in values {
        *counts.entry(value.to_owned()).or_default() += 1;
    }

    counts
        .into_iter()
        .map(|(status, count)| StatusCount { status, count })
        .collect()
}

fn grove_status_label(status: GroveBeadStatus) -> &'static str {
    match status {
        GroveBeadStatus::Idle => "Idle",
        GroveBeadStatus::Ready => "Ready",
        GroveBeadStatus::Running => "Running",
        GroveBeadStatus::Checkpointed => "Checkpointed",
        GroveBeadStatus::WaitingToRetry => "WaitingToRetry",
        GroveBeadStatus::Succeeded => "Succeeded",
        GroveBeadStatus::Failed => "Failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grove_types::{BeadRef, CircuitState, RunId, Timestamp};
    use std::error::Error;

    type TestResult<T = ()> = Result<T, Box<dyn Error>>;

    #[test]
    fn counts_br_and_grove_statuses_separately() -> TestResult {
        let beads = vec![
            sample_bead("grove-1", "open", GroveBeadStatus::Ready)?,
            sample_bead("grove-2", "open", GroveBeadStatus::Running)?,
            sample_bead("grove-3", "closed", GroveBeadStatus::Succeeded)?,
        ];

        let bead_counts = count_beads_statuses(&beads);
        let grove_counts = count_grove_statuses(&beads);

        assert_eq!(bead_counts.len(), 2);
        assert_eq!(bead_counts[0].status, "closed");
        assert_eq!(bead_counts[0].count, 1);
        assert_eq!(bead_counts[1].status, "open");
        assert_eq!(bead_counts[1].count, 2);

        assert_eq!(grove_counts.len(), 3);
        assert_eq!(grove_counts[0].status, "Ready");
        assert_eq!(grove_counts[1].status, "Running");
        assert_eq!(grove_counts[2].status, "Succeeded");
        Ok(())
    }

    #[test]
    fn dispatch_explanation_summarizes_local_suppressions() -> TestResult {
        let bead = sample_bead("grove-4", "open", GroveBeadStatus::WaitingToRetry)?;
        let eligibility = crate::evaluate_dispatch_eligibility(
            &bead,
            &crate::DispatchEligibilityContext {
                ready_in_br: true,
                circuit_state: CircuitState::Closed,
                reservation_conflicts: Vec::new(),
                now: parse_ts("2026-03-16T12:00:00Z")?,
            },
        );

        let view = DispatchExplanationView::from_eligibility(&eligibility);

        assert!(!view.dispatchable_in_grove);
        assert_eq!(view.summary(), "retry backoff still pending");
        assert_eq!(view.local_suppression_reasons[0].code, "retry_backoff_pending");
        Ok(())
    }

    #[test]
    fn suppression_reason_carries_reservation_conflict_details() {
        let reason = LocalSuppressionReason::ReservationConflict {
            conflict: ReservationConflict {
                requested_by_bead: BeadId::new("grove-req"),
                conflicting_bead: BeadId::new("grove-held"),
                requested_pattern: "src/**".to_owned(),
                held_pattern: "src/lib.rs".to_owned(),
                conflicting_run_id: Some(RunId::new("run-7")),
            },
        };

        let view = SuppressionReasonView::from_reason(&reason);

        assert_eq!(view.code, "reservation_conflict");
        assert!(view.summary.contains("grove-held"));
        assert_eq!(
            view.conflict.as_ref().map(|conflict| conflict.held_pattern.as_str()),
            Some("src/lib.rs")
        );
    }

    fn sample_bead(
        id: &str,
        br_status: &str,
        grove_status: GroveBeadStatus,
    ) -> TestResult<GroveBeadRecord> {
        let created_at = parse_ts("2026-03-16T10:00:00Z")?;
        let updated_at = parse_ts("2026-03-16T11:00:00Z")?;
        let retry_after = match grove_status {
            GroveBeadStatus::WaitingToRetry => Some(parse_ts("2026-03-16T12:30:00Z")?),
            _ => None,
        };

        Ok(GroveBeadRecord {
            bead: BeadRef {
                id: BeadId::new(id),
                title: format!("title-{id}"),
                description: None,
                priority: BeadPriority::P1,
                issue_type: "task".to_owned(),
                br_status: br_status.to_owned(),
                assignee: None,
                labels: Vec::new(),
                created_at,
                updated_at,
            },
            grove_status,
            declared_paths: Vec::new(),
            metadata: Default::default(),
            last_run_id: None,
            retry_after,
            last_failure_class: None,
            last_failure_detail: None,
            synced_at: updated_at,
            runtime_updated_at: updated_at,
        })
    }

    fn parse_ts(value: &str) -> TestResult<Timestamp> {
        Ok(value.parse()?)
    }
}
