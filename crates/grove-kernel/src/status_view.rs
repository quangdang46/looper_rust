use crate::{DispatchEligibility, DispatchEligibilityContext, LocalSuppressionReason};
use anyhow::Result;
use chrono::Utc;
use grove_br::{BrClient, BrDependencySnapshot};
use grove_config::GroveConfig;
use grove_db::Database;
use grove_types::{
    BeadId, BeadPriority, FailureClass, GroveBeadRecord, GroveBeadStatus, ReservationConflict,
    ReservationMode, ReservationRecord, RunId, SessionId, Timestamp,
};
use std::collections::{BTreeMap, HashMap, HashSet};

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
                retry_after: *retry_after,
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
    count_strings(
        beads
            .iter()
            .map(|bead| grove_status_label(bead.grove_status)),
    )
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

pub fn load_status_snapshot<C: BrClient>(
    db: &Database,
    br: &C,
    workspace_root: &str,
    config: &GroveConfig,
) -> Result<StatusSnapshot> {
    let beads = db.list_bead_records()?;
    let ready_ids = br
        .ready()?
        .into_iter()
        .map(|bead| bead.id)
        .collect::<HashSet<_>>();
    let reservations = db.list_active_reservations()?;
    let reservation_map = reservations_by_bead(&reservations);
    let reservation_conflicts = find_reservation_conflicts(&reservations);
    let mirror_pending_map = mirror_pending_by_bead(&beads, db)?;
    let dependency_map = dependency_snapshots_by_bead(&beads, db)?;

    let running_beads = build_running_beads(&beads, db)?;
    let ready_queue = build_ready_queue(
        &beads,
        &ready_ids,
        &dependency_map,
        &reservation_conflicts,
        &mirror_pending_map,
        config,
    );
    let checkpointed_beads = build_checkpointed_beads(&beads, db, &reservation_map)?;
    let failed_beads = build_failed_beads(
        &beads,
        &ready_ids,
        &reservation_conflicts,
        &mirror_pending_map,
        config,
    )?;

    Ok(StatusSnapshot {
        workspace_root: workspace_root.to_owned(),
        leader: None,
        beads,
        running_beads,
        ready_queue,
        checkpointed_beads,
        failed_beads,
        reservation_conflicts: reservation_conflicts
            .iter()
            .map(ReservationConflictView::from_conflict)
            .collect(),
        mirror_pending: mirror_pending_map.into_values().collect(),
    })
}

fn build_running_beads(beads: &[GroveBeadRecord], db: &Database) -> Result<Vec<RunningBeadView>> {
    beads
        .iter()
        .filter(|bead| bead.grove_status == GroveBeadStatus::Running)
        .map(|bead| {
            let latest_session = bead
                .last_run_id
                .as_ref()
                .map(|run_id| db.latest_session_for_run(run_id))
                .transpose()?
                .flatten();
            Ok(RunningBeadView {
                bead_id: bead.bead.id.clone(),
                title: bead.bead.title.clone(),
                priority: bead.bead.priority,
                br_status: bead.bead.br_status.clone(),
                grove_status: bead.grove_status,
                run_id: bead.last_run_id.clone(),
                session_id: latest_session.as_ref().map(|session| session.id.clone()),
                started_at: latest_session.as_ref().map(|session| session.started_at),
                context_pressure_pct: None,
                last_progress: None,
            })
        })
        .collect()
}

fn build_ready_queue(
    beads: &[GroveBeadRecord],
    ready_ids: &HashSet<BeadId>,
    dependency_map: &HashMap<BeadId, BrDependencySnapshot>,
    reservation_conflicts: &[ReservationConflict],
    mirror_pending_map: &HashMap<BeadId, MirrorPendingView>,
    config: &GroveConfig,
) -> Vec<ReadyQueueEntry> {
    let now = Utc::now();
    let mut entries = beads
        .iter()
        .filter_map(|bead| {
            let conflicts = conflicts_for_bead(&bead.bead.id, reservation_conflicts);
            let dependency_snapshot = dependency_map.get(&bead.bead.id);
            let eligibility = crate::evaluate_dispatch_eligibility(
                bead,
                &DispatchEligibilityContext {
                    ready_in_br: ready_ids.contains(&bead.bead.id),
                    circuit_state: grove_types::CircuitState::Closed,
                    reservation_conflicts: conflicts.clone(),
                    now,
                },
            );
            let dispatch = DispatchExplanationView::from_eligibility(&eligibility);
            if !dispatch.ready_in_br {
                return None;
            }

            let score_breakdown =
                compute_score_breakdown(bead, dependency_snapshot, conflicts.len(), config);
            let score = score_breakdown
                .iter()
                .map(|component| component.value)
                .sum::<f64>();
            let dependent_count = dependency_snapshot.map_or(0, |snapshot| snapshot.blocks.len());
            let mut why = vec![priority_why(bead.bead.priority)];
            if dependent_count > 0 {
                why.push(format!(
                    "{} downstream bead{}",
                    dependent_count,
                    if dependent_count == 1 { "" } else { "s" }
                ));
            }
            if conflicts.is_empty() {
                why.push("no reservation conflicts".to_owned());
            } else {
                why.push(format!("{} reservation conflict(s)", conflicts.len()));
            }

            Some(ReadyQueueEntry {
                bead_id: bead.bead.id.clone(),
                title: bead.bead.title.clone(),
                priority: bead.bead.priority,
                score: Some(score),
                score_breakdown,
                why,
                dispatch,
                mirror_pending: mirror_pending_map.contains_key(&bead.bead.id),
            })
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.bead_id.cmp(&right.bead_id))
    });
    entries
}

fn build_checkpointed_beads(
    beads: &[GroveBeadRecord],
    db: &Database,
    reservation_map: &HashMap<BeadId, Vec<ReservationRecord>>,
) -> Result<Vec<CheckpointedBeadView>> {
    beads
        .iter()
        .filter(|bead| bead.grove_status == GroveBeadStatus::Checkpointed)
        .map(|bead| {
            let checkpoint = db.latest_checkpoint_for_bead(&bead.bead.id)?;
            let claimed_paths = checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.payload.get("claimed_paths"))
                .and_then(|value| value.as_array())
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                        .collect()
                })
                .unwrap_or_else(|| {
                    reservation_map
                        .get(&bead.bead.id)
                        .into_iter()
                        .flat_map(|records| {
                            records.iter().map(|record| record.path_pattern.clone())
                        })
                        .collect()
                });

            Ok(CheckpointedBeadView {
                bead_id: bead.bead.id.clone(),
                title: bead.bead.title.clone(),
                run_id: bead.last_run_id.clone(),
                checkpoint_id: checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.id.to_string()),
                progress: checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.progress.clone()),
                next_step: checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.next_step.clone()),
                claimed_paths,
                saved_at: checkpoint.as_ref().map(|checkpoint| checkpoint.saved_at),
            })
        })
        .collect()
}

fn build_failed_beads(
    beads: &[GroveBeadRecord],
    ready_ids: &HashSet<BeadId>,
    reservation_conflicts: &[ReservationConflict],
    mirror_pending_map: &HashMap<BeadId, MirrorPendingView>,
    config: &GroveConfig,
) -> Result<Vec<FailedBeadView>> {
    let now = Utc::now();
    let mut failed = Vec::new();
    for bead in beads.iter().filter(|bead| {
        matches!(
            bead.grove_status,
            GroveBeadStatus::Failed | GroveBeadStatus::WaitingToRetry
        )
    }) {
        let conflicts = conflicts_for_bead(&bead.bead.id, reservation_conflicts);
        let eligibility = crate::evaluate_dispatch_eligibility(
            bead,
            &DispatchEligibilityContext {
                ready_in_br: ready_ids.contains(&bead.bead.id),
                circuit_state: grove_types::CircuitState::Closed,
                reservation_conflicts: conflicts,
                now,
            },
        );
        let dispatch = ready_ids
            .contains(&bead.bead.id)
            .then(|| DispatchExplanationView::from_eligibility(&eligibility));

        failed.push(FailedBeadView {
            bead_id: bead.bead.id.clone(),
            title: bead.bead.title.clone(),
            priority: bead.bead.priority,
            run_id: bead.last_run_id.clone(),
            failure_class: bead.last_failure_class,
            failure_detail: bead.last_failure_detail.clone(),
            retry_after: bead.retry_after,
            dispatch,
            recovery_hint: recovery_hint(bead, config),
            mirror_pending: mirror_pending_map.contains_key(&bead.bead.id),
        });
    }

    failed.sort_by(|left, right| left.bead_id.cmp(&right.bead_id));
    Ok(failed)
}

fn dependency_snapshots_by_bead(
    beads: &[GroveBeadRecord],
    db: &Database,
) -> Result<HashMap<BeadId, BrDependencySnapshot>> {
    beads
        .iter()
        .map(|bead| {
            db.dependency_snapshot(&bead.bead.id)
                .map(|snapshot| (bead.bead.id.clone(), snapshot))
        })
        .collect()
}

fn reservations_by_bead(
    reservations: &[ReservationRecord],
) -> HashMap<BeadId, Vec<ReservationRecord>> {
    let mut reservations_by_bead = HashMap::<BeadId, Vec<ReservationRecord>>::new();
    for reservation in reservations {
        reservations_by_bead
            .entry(reservation.bead_id.clone())
            .or_default()
            .push(reservation.clone());
    }
    reservations_by_bead
}

fn mirror_pending_by_bead(
    beads: &[GroveBeadRecord],
    db: &Database,
) -> Result<HashMap<BeadId, MirrorPendingView>> {
    let mut map = HashMap::new();
    for bead in beads {
        if let Some(pending) = latest_mirror_pending_for_bead(&bead.bead.id, db)? {
            map.insert(bead.bead.id.clone(), pending);
        }
    }
    Ok(map)
}

pub(crate) fn latest_mirror_pending_for_bead(
    bead_id: &BeadId,
    db: &Database,
) -> Result<Option<MirrorPendingView>> {
    let events = db.list_event_logs_for_bead(bead_id)?;
    let latest_terminal = events.iter().find(|event| {
        matches!(
            event.kind,
            grove_types::EventKind::BrMirrorSucceeded | grove_types::EventKind::BrMirrorFailed
        )
    });

    match latest_terminal {
        Some(event) if matches!(event.kind, grove_types::EventKind::BrMirrorFailed) => {
            Ok(Some(MirrorPendingView {
                bead_id: bead_id.clone(),
                run_id: event.run_id.clone(),
                pending_actions: vec!["close".to_owned()],
                last_attempt_at: Some(event.created_at),
                last_error: event
                    .payload
                    .get("error")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
            }))
        }
        _ => Ok(None),
    }
}

pub(crate) fn find_reservation_conflicts(
    reservations: &[ReservationRecord],
) -> Vec<ReservationConflict> {
    let mut conflicts = Vec::new();
    for (index, left) in reservations.iter().enumerate() {
        for right in reservations.iter().skip(index + 1) {
            if left.bead_id == right.bead_id {
                continue;
            }
            if left.mode != ReservationMode::Exclusive && right.mode != ReservationMode::Exclusive {
                continue;
            }
            if patterns_overlap(&left.path_pattern, &right.path_pattern) {
                conflicts.push(ReservationConflict {
                    requested_by_bead: left.bead_id.clone(),
                    conflicting_bead: right.bead_id.clone(),
                    requested_pattern: left.path_pattern.clone(),
                    held_pattern: right.path_pattern.clone(),
                    conflicting_run_id: right.run_id.clone(),
                });
                conflicts.push(ReservationConflict {
                    requested_by_bead: right.bead_id.clone(),
                    conflicting_bead: left.bead_id.clone(),
                    requested_pattern: right.path_pattern.clone(),
                    held_pattern: left.path_pattern.clone(),
                    conflicting_run_id: left.run_id.clone(),
                });
            }
        }
    }
    conflicts
}

pub(crate) fn conflicts_for_bead(
    bead_id: &BeadId,
    reservation_conflicts: &[ReservationConflict],
) -> Vec<ReservationConflict> {
    reservation_conflicts
        .iter()
        .filter(|conflict| &conflict.requested_by_bead == bead_id)
        .cloned()
        .collect()
}

fn patterns_overlap(left: &str, right: &str) -> bool {
    left == right
        || left.starts_with(right.trim_end_matches('*'))
        || right.starts_with(left.trim_end_matches('*'))
        || left.contains("**") && right.starts_with(left.split("**").next().unwrap_or_default())
        || right.contains("**") && left.starts_with(right.split("**").next().unwrap_or_default())
}

fn compute_score_breakdown(
    bead: &GroveBeadRecord,
    dependency_snapshot: Option<&BrDependencySnapshot>,
    conflict_count: usize,
    config: &GroveConfig,
) -> Vec<ScoreComponentView> {
    let mut breakdown = vec![ScoreComponentView {
        label: "priority".to_owned(),
        value: priority_score(bead.bead.priority),
        note: Some(priority_why(bead.bead.priority)),
    }];

    let dependent_count = dependency_snapshot.map_or(0, |snapshot| snapshot.blocks.len());
    if dependent_count > 0 {
        breakdown.push(ScoreComponentView {
            label: "critical_path".to_owned(),
            value: f64::from(config.scheduler.critical_path_bonus),
            note: Some(format!("{} downstream bead(s)", dependent_count)),
        });
    }

    if bead.grove_status == GroveBeadStatus::WaitingToRetry {
        breakdown.push(ScoreComponentView {
            label: "retry_penalty".to_owned(),
            value: -f64::from(config.scheduler.retry_penalty),
            note: Some("waiting to retry".to_owned()),
        });
    }

    if conflict_count > 0 {
        breakdown.push(ScoreComponentView {
            label: "reservation_conflict_penalty".to_owned(),
            value: -f64::from(config.scheduler.reservation_conflict_penalty),
            note: Some(format!("{} active conflict(s)", conflict_count)),
        });
    }

    breakdown
}

fn priority_score(priority: BeadPriority) -> f64 {
    match priority {
        BeadPriority::P0 => 100.0,
        BeadPriority::P1 => 75.0,
        BeadPriority::P2 => 50.0,
        BeadPriority::P3 => 25.0,
        BeadPriority::P4 => 10.0,
    }
}

fn priority_why(priority: BeadPriority) -> String {
    match priority {
        BeadPriority::P0 => "P0 priority".to_owned(),
        BeadPriority::P1 => "P1 priority".to_owned(),
        BeadPriority::P2 => "P2 priority".to_owned(),
        BeadPriority::P3 => "P3 priority".to_owned(),
        BeadPriority::P4 => "P4 priority".to_owned(),
    }
}

fn recovery_hint(bead: &GroveBeadRecord, config: &GroveConfig) -> Option<String> {
    match bead.grove_status {
        GroveBeadStatus::WaitingToRetry => bead.retry_after.map(|retry_after| {
            format!(
                "automatic retry available after {retry_after} (retry max {})",
                config.scheduler.retry_max
            )
        }),
        GroveBeadStatus::Failed => {
            Some("run `grove retry <bead-id>` after reviewing the last failure".to_owned())
        }
        _ => None,
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
        assert_eq!(
            view.local_suppression_reasons[0].code,
            "retry_backoff_pending"
        );
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
            view.conflict
                .as_ref()
                .map(|conflict| conflict.held_pattern.as_str()),
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
