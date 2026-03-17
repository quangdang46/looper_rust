pub mod inspect_view;
pub mod status_view;

use anyhow::Result;
use grove_br::{BrClient, BrDependencySnapshot};
use grove_config::GroveConfig;
use grove_db::Database;
use grove_types::{
    BeadId, CircuitState, GroveBeadRecord, GroveBeadStatus, ReservationConflict, RunId, Timestamp,
};
use std::collections::BTreeMap;

pub use inspect_view::BeadInspectView;
pub use status_view::WorkspaceStatusView;

pub const CRATE_PURPOSE: &str = "Core Grove runtime domain and service boundaries.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySnapshotIssue {
    SelfBlockedBy,
    SelfBlocks,
    DuplicateBlockedBy { bead_id: BeadId, occurrences: usize },
    DuplicateBlocks { bead_id: BeadId, occurrences: usize },
}

impl DependencySnapshotIssue {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::SelfBlockedBy => "self_blocked_by",
            Self::SelfBlocks => "self_blocks",
            Self::DuplicateBlockedBy { .. } => "duplicate_blocked_by",
            Self::DuplicateBlocks { .. } => "duplicate_blocks",
        }
    }

    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::SelfBlockedBy => {
                "cached dependency snapshot lists the bead as blocking itself".to_owned()
            }
            Self::SelfBlocks => {
                "cached dependency snapshot lists the bead as its own dependent".to_owned()
            }
            Self::DuplicateBlockedBy {
                bead_id,
                occurrences,
            } => format!(
                "cached dependency snapshot repeats blocker {} {} times",
                bead_id.as_str(),
                occurrences
            ),
            Self::DuplicateBlocks {
                bead_id,
                occurrences,
            } => format!(
                "cached dependency snapshot repeats dependent {} {} times",
                bead_id.as_str(),
                occurrences
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DependencySnapshotSanity {
    pub snapshot: BrDependencySnapshot,
    pub issues: Vec<DependencySnapshotIssue>,
}

impl DependencySnapshotSanity {
    #[must_use]
    pub fn is_sane(&self) -> bool {
        self.issues.is_empty()
    }
}

#[must_use]
pub fn validate_dependency_snapshot(
    snapshot: &BrDependencySnapshot,
) -> Vec<DependencySnapshotIssue> {
    let mut issues = Vec::new();

    if snapshot.blocked_by.iter().any(|id| id == &snapshot.bead_id) {
        issues.push(DependencySnapshotIssue::SelfBlockedBy);
    }

    if snapshot.blocks.iter().any(|id| id == &snapshot.bead_id) {
        issues.push(DependencySnapshotIssue::SelfBlocks);
    }

    issues.extend(
        duplicate_dependency_ids(&snapshot.blocked_by)
            .into_iter()
            .map(
                |(bead_id, occurrences)| DependencySnapshotIssue::DuplicateBlockedBy {
                    bead_id,
                    occurrences,
                },
            ),
    );
    issues.extend(duplicate_dependency_ids(&snapshot.blocks).into_iter().map(
        |(bead_id, occurrences)| DependencySnapshotIssue::DuplicateBlocks {
            bead_id,
            occurrences,
        },
    ));

    issues
}

pub fn inspect_dependency_snapshot(
    db: &Database,
    bead_id: &BeadId,
) -> Result<Option<DependencySnapshotSanity>> {
    if db.get_bead_record(bead_id)?.is_none() {
        return Ok(None);
    }

    let snapshot = db.dependency_snapshot(bead_id)?;
    let issues = validate_dependency_snapshot(&snapshot);
    Ok(Some(DependencySnapshotSanity { snapshot, issues }))
}

pub fn load_workspace_status_view<C: BrClient>(
    db: &Database,
    br: &C,
    workspace_root: &str,
    config: &GroveConfig,
) -> Result<WorkspaceStatusView> {
    Ok(status_view::load_status_snapshot(db, br, workspace_root, config)?.into_view())
}

pub fn load_bead_inspect_view<C: BrClient>(
    db: &Database,
    br: &C,
    bead_id: &BeadId,
    config: &GroveConfig,
) -> Result<Option<BeadInspectView>> {
    Ok(
        inspect_view::load_inspect_snapshot(db, br, bead_id, config)?
            .map(|snapshot| snapshot.into_view()),
    )
}

fn duplicate_dependency_ids(ids: &[BeadId]) -> Vec<(BeadId, usize)> {
    let mut counts = BTreeMap::<String, usize>::new();
    for bead_id in ids {
        *counts.entry(bead_id.as_str().to_owned()).or_default() += 1;
    }

    counts
        .into_iter()
        .filter(|(_, occurrences)| *occurrences > 1)
        .map(|(bead_id, occurrences)| (BeadId::new(bead_id), occurrences))
        .collect()
}

#[derive(Debug, Clone)]
pub struct DispatchEligibilityContext {
    pub ready_in_br: bool,
    pub circuit_state: CircuitState,
    pub reservation_conflicts: Vec<ReservationConflict>,
    pub now: Timestamp,
}

#[derive(Debug, Clone)]
pub struct DispatchEligibility {
    pub ready_in_br: bool,
    pub dispatchable_in_grove: bool,
    pub local_suppression_reasons: Vec<LocalSuppressionReason>,
}

impl DispatchEligibility {
    #[must_use]
    pub fn has_local_suppressions(&self) -> bool {
        !self.local_suppression_reasons.is_empty()
    }
}

#[derive(Debug, Clone)]
pub enum LocalSuppressionReason {
    SuppressedByLabel { label: String },
    NonExecutableIssueType { issue_type: String },
    ActiveRun { run_id: Option<RunId> },
    CheckpointPendingResume { run_id: Option<RunId> },
    RetryBackoffPending { retry_after: Option<Timestamp> },
    CircuitOpen,
    ReservationConflict { conflict: ReservationConflict },
    AlreadySucceeded,
    FailedAwaitingManualRetry,
}

impl LocalSuppressionReason {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::SuppressedByLabel { .. } => "suppressed_by_label",
            Self::NonExecutableIssueType { .. } => "non_executable_issue_type",
            Self::ActiveRun { .. } => "active_run",
            Self::CheckpointPendingResume { .. } => "checkpoint_pending_resume",
            Self::RetryBackoffPending { .. } => "retry_backoff_pending",
            Self::CircuitOpen => "circuit_open",
            Self::ReservationConflict { .. } => "reservation_conflict",
            Self::AlreadySucceeded => "already_succeeded",
            Self::FailedAwaitingManualRetry => "failed_awaiting_manual_retry",
        }
    }
}

#[must_use]
pub fn evaluate_dispatch_eligibility(
    bead: &GroveBeadRecord,
    context: &DispatchEligibilityContext,
) -> DispatchEligibility {
    let local_suppression_reasons = collect_local_suppressions(bead, context);
    let dispatchable_in_grove = context.ready_in_br && local_suppression_reasons.is_empty();

    DispatchEligibility {
        ready_in_br: context.ready_in_br,
        dispatchable_in_grove,
        local_suppression_reasons,
    }
}

#[must_use]
pub fn dispatch_suppression_label(labels: &[String]) -> Option<String> {
    labels
        .iter()
        .find(|label| label.eq_ignore_ascii_case("dispatch:no"))
        .cloned()
}

#[must_use]
pub fn is_non_executable_issue_type(issue_type: &str) -> bool {
    matches!(
        issue_type.trim().to_ascii_lowercase().as_str(),
        "epic" | "tracking"
    )
}

fn collect_local_suppressions(
    bead: &GroveBeadRecord,
    context: &DispatchEligibilityContext,
) -> Vec<LocalSuppressionReason> {
    let mut reasons = Vec::new();

    if let Some(label) = dispatch_suppression_label(&bead.bead.labels) {
        reasons.push(LocalSuppressionReason::SuppressedByLabel { label });
    }

    if is_non_executable_issue_type(&bead.bead.issue_type) {
        reasons.push(LocalSuppressionReason::NonExecutableIssueType {
            issue_type: bead.bead.issue_type.clone(),
        });
    }

    match bead.grove_status {
        GroveBeadStatus::Idle | GroveBeadStatus::Ready => {}
        GroveBeadStatus::Running => reasons.push(LocalSuppressionReason::ActiveRun {
            run_id: bead.last_run_id.clone(),
        }),
        GroveBeadStatus::Checkpointed => {
            reasons.push(LocalSuppressionReason::CheckpointPendingResume {
                run_id: bead.last_run_id.clone(),
            });
        }
        GroveBeadStatus::WaitingToRetry => {
            if bead.retry_after.is_none()
                || bead
                    .retry_after
                    .as_ref()
                    .is_some_and(|ts| ts > &context.now)
            {
                reasons.push(LocalSuppressionReason::RetryBackoffPending {
                    retry_after: bead.retry_after,
                });
            }
        }
        GroveBeadStatus::Succeeded => reasons.push(LocalSuppressionReason::AlreadySucceeded),
        GroveBeadStatus::Failed => reasons.push(LocalSuppressionReason::FailedAwaitingManualRetry),
    }

    if matches!(context.circuit_state, CircuitState::Open) {
        reasons.push(LocalSuppressionReason::CircuitOpen);
    }

    reasons.extend(
        context
            .reservation_conflicts
            .iter()
            .cloned()
            .map(|conflict| LocalSuppressionReason::ReservationConflict { conflict }),
    );

    reasons
}

#[cfg(test)]
mod tests {
    use super::*;
    use grove_types::{BeadId, BeadPriority, BeadRef, RunId};
    use std::error::Error;

    type TestResult<T = ()> = Result<T, Box<dyn Error>>;

    #[test]
    fn ready_bead_without_local_blockers_is_dispatchable() -> TestResult {
        let bead = sample_bead(GroveBeadStatus::Ready, "task", &[], None, None)?;
        let context = sample_context(true, CircuitState::Closed, Vec::new())?;

        let eligibility = evaluate_dispatch_eligibility(&bead, &context);

        assert!(eligibility.ready_in_br);
        assert!(eligibility.dispatchable_in_grove);
        assert!(!eligibility.has_local_suppressions());
        Ok(())
    }

    #[test]
    fn not_ready_in_br_never_becomes_dispatchable() -> TestResult {
        let bead = sample_bead(GroveBeadStatus::Ready, "task", &[], None, None)?;
        let context = sample_context(false, CircuitState::Closed, Vec::new())?;

        let eligibility = evaluate_dispatch_eligibility(&bead, &context);

        assert!(!eligibility.ready_in_br);
        assert!(!eligibility.dispatchable_in_grove);
        assert!(eligibility.local_suppression_reasons.is_empty());
        Ok(())
    }

    #[test]
    fn label_and_issue_type_can_both_suppress_dispatch() -> TestResult {
        let bead = sample_bead(GroveBeadStatus::Ready, "epic", &["dispatch:no"], None, None)?;
        let context = sample_context(true, CircuitState::Closed, Vec::new())?;

        let eligibility = evaluate_dispatch_eligibility(&bead, &context);
        let reason_codes = suppression_codes(&eligibility);

        assert!(!eligibility.dispatchable_in_grove);
        assert!(reason_codes.contains(&"suppressed_by_label"));
        assert!(reason_codes.contains(&"non_executable_issue_type"));
        Ok(())
    }

    #[test]
    fn active_run_status_suppresses_dispatch() -> TestResult {
        let bead = sample_bead(GroveBeadStatus::Running, "task", &[], Some("run_123"), None)?;
        let context = sample_context(true, CircuitState::Closed, Vec::new())?;

        let eligibility = evaluate_dispatch_eligibility(&bead, &context);

        assert!(!eligibility.dispatchable_in_grove);
        assert!(suppression_codes(&eligibility).contains(&"active_run"));
        Ok(())
    }

    #[test]
    fn checkpointed_status_suppresses_dispatch() -> TestResult {
        let bead = sample_bead(
            GroveBeadStatus::Checkpointed,
            "task",
            &[],
            Some("run_456"),
            None,
        )?;
        let context = sample_context(true, CircuitState::Closed, Vec::new())?;

        let eligibility = evaluate_dispatch_eligibility(&bead, &context);

        assert!(!eligibility.dispatchable_in_grove);
        assert!(suppression_codes(&eligibility).contains(&"checkpoint_pending_resume"));
        Ok(())
    }

    #[test]
    fn retry_backoff_only_suppresses_while_timer_is_pending() -> TestResult {
        let blocked = sample_bead(
            GroveBeadStatus::WaitingToRetry,
            "task",
            &[],
            None,
            Some("2026-03-16T12:30:00Z"),
        )?;
        let expired = sample_bead(
            GroveBeadStatus::WaitingToRetry,
            "task",
            &[],
            None,
            Some("2026-03-16T11:30:00Z"),
        )?;
        let context = sample_context(true, CircuitState::Closed, Vec::new())?;

        let blocked_eligibility = evaluate_dispatch_eligibility(&blocked, &context);
        let expired_eligibility = evaluate_dispatch_eligibility(&expired, &context);

        assert!(suppression_codes(&blocked_eligibility).contains(&"retry_backoff_pending"));
        assert!(blocked_eligibility.has_local_suppressions());
        assert!(!expired_eligibility.has_local_suppressions());
        assert!(expired_eligibility.dispatchable_in_grove);
        Ok(())
    }

    #[test]
    fn circuit_open_and_reservation_conflict_suppress_dispatch() -> TestResult {
        let bead = sample_bead(GroveBeadStatus::Ready, "task", &[], None, None)?;
        let conflict = ReservationConflict {
            requested_by_bead: BeadId::new("grove-1j9.5.10"),
            conflicting_bead: BeadId::new("grove-1j9.5.4"),
            requested_pattern: "crates/grove-kernel/**".into(),
            held_pattern: "crates/grove-kernel/src/lib.rs".into(),
            conflicting_run_id: Some(RunId::new("run_conflict")),
        };
        let context = sample_context(true, CircuitState::Open, vec![conflict])?;

        let eligibility = evaluate_dispatch_eligibility(&bead, &context);
        let reason_codes = suppression_codes(&eligibility);

        assert!(!eligibility.dispatchable_in_grove);
        assert!(reason_codes.contains(&"circuit_open"));
        assert!(reason_codes.contains(&"reservation_conflict"));
        Ok(())
    }

    #[test]
    fn succeeded_and_failed_beads_are_not_dispatchable() -> TestResult {
        let succeeded = sample_bead(GroveBeadStatus::Succeeded, "task", &[], None, None)?;
        let failed = sample_bead(GroveBeadStatus::Failed, "task", &[], None, None)?;
        let context = sample_context(true, CircuitState::Closed, Vec::new())?;

        let succeeded_eligibility = evaluate_dispatch_eligibility(&succeeded, &context);
        let failed_eligibility = evaluate_dispatch_eligibility(&failed, &context);

        assert!(suppression_codes(&succeeded_eligibility).contains(&"already_succeeded"));
        assert!(suppression_codes(&failed_eligibility).contains(&"failed_awaiting_manual_retry"));
        Ok(())
    }

    #[test]
    fn dependency_snapshot_sanity_detects_self_edges_and_duplicates() {
        let snapshot = BrDependencySnapshot {
            bead_id: BeadId::new("grove-1"),
            blocked_by: vec![
                BeadId::new("grove-parent"),
                BeadId::new("grove-1"),
                BeadId::new("grove-parent"),
            ],
            blocks: vec![
                BeadId::new("grove-1"),
                BeadId::new("grove-child"),
                BeadId::new("grove-child"),
            ],
            rows: Vec::new(),
        };

        let issues = validate_dependency_snapshot(&snapshot);
        let codes: Vec<_> = issues.iter().map(DependencySnapshotIssue::code).collect();

        assert!(codes.contains(&"self_blocked_by"));
        assert!(codes.contains(&"self_blocks"));
        assert!(codes.contains(&"duplicate_blocked_by"));
        assert!(codes.contains(&"duplicate_blocks"));
    }

    #[test]
    fn dependency_snapshot_sanity_accepts_unique_non_self_edges() {
        let snapshot = BrDependencySnapshot {
            bead_id: BeadId::new("grove-1"),
            blocked_by: vec![BeadId::new("grove-parent")],
            blocks: vec![BeadId::new("grove-child")],
            rows: Vec::new(),
        };

        let sanity = DependencySnapshotSanity {
            snapshot: snapshot.clone(),
            issues: validate_dependency_snapshot(&snapshot),
        };

        assert!(sanity.is_sane());
        assert!(sanity.issues.is_empty());
    }

    fn sample_context(
        ready_in_br: bool,
        circuit_state: CircuitState,
        reservation_conflicts: Vec<ReservationConflict>,
    ) -> TestResult<DispatchEligibilityContext> {
        Ok(DispatchEligibilityContext {
            ready_in_br,
            circuit_state,
            reservation_conflicts,
            now: "2026-03-16T12:00:00Z".parse()?,
        })
    }

    fn sample_bead(
        grove_status: GroveBeadStatus,
        issue_type: &str,
        labels: &[&str],
        last_run_id: Option<&str>,
        retry_after: Option<&str>,
    ) -> TestResult<GroveBeadRecord> {
        let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
        let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;

        Ok(GroveBeadRecord {
            bead: BeadRef {
                id: BeadId::new("grove-1j9.5.10"),
                title: "dispatch policy".into(),
                description: None,
                priority: BeadPriority::P0,
                issue_type: issue_type.into(),
                br_status: "open".into(),
                assignee: None,
                labels: labels.iter().map(|label| (*label).to_owned()).collect(),
                created_at,
                updated_at,
            },
            grove_status,
            declared_paths: Vec::new(),
            metadata: Default::default(),
            last_run_id: last_run_id.map(RunId::new),
            retry_after: retry_after.map(str::parse).transpose()?,
            last_failure_class: None,
            last_failure_detail: None,
            synced_at: updated_at,
            runtime_updated_at: updated_at,
        })
    }

    fn suppression_codes(eligibility: &DispatchEligibility) -> Vec<&'static str> {
        eligibility
            .local_suppression_reasons
            .iter()
            .map(LocalSuppressionReason::code)
            .collect()
    }
}
