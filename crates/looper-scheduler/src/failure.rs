use std::time::Duration;

use looper_storage::record::QueueItemRecord;

use crate::error::SchedulerError;
use crate::types::{FailureBoundary, FailureClassificationContext, QueueFailureKind};

// ---------------------------------------------------------------------------
// Long-term retry threshold
// ---------------------------------------------------------------------------

pub const QUEUE_LONG_TERM_RETRY_ATTEMPT_THRESHOLD: i64 = 5;

// ---------------------------------------------------------------------------
// classify_failure — main classification entry point
// ---------------------------------------------------------------------------

/// Classify an error into a `QueueFailureKind`.
pub fn classify_failure(err: &SchedulerError, ctx: &FailureClassificationContext) -> QueueFailureKind {
    // 1. Direct boundary-based classification
    classify_by_boundary(err, ctx)
}

/// Classify based on the failure boundary and error message patterns.
pub fn classify_by_boundary(err: &SchedulerError, ctx: &FailureClassificationContext) -> QueueFailureKind {
    let boundary = ctx.boundary;
    let msg = err.to_string().to_lowercase();

    // Dirty worktree → ManualIntervention
    if is_manual_worktree_message(&msg) || boundary == FailureBoundary::LocalWorktree {
        return QueueFailureKind::ManualIntervention;
    }

    // GitHub API unauthorized → RetryableTransient
    if boundary == FailureBoundary::GitHubAPI && is_gql_unauthorized(&msg) {
        return QueueFailureKind::RetryableTransient;
    }

    // Deterministic denial → NonRetryable
    if is_deterministic_denial(&msg) {
        return QueueFailureKind::NonRetryable;
    }

    // GitHub API 400/422 → NonRetryable
    if boundary == FailureBoundary::GitHubAPI && is_http_4xx_denial(&msg) {
        return QueueFailureKind::NonRetryable;
    }

    // Internal deterministic boundaries → NonRetryable
    if is_internal_deterministic_boundary(boundary) {
        return QueueFailureKind::NonRetryable;
    }

    // External boundaries → RetryableTransient
    if is_external_boundary(boundary) {
        return QueueFailureKind::RetryableTransient;
    }

    // Fallback
    QueueFailureKind::NonRetryable
}

fn is_external_boundary(boundary: FailureBoundary) -> bool {
    matches!(
        boundary,
        FailureBoundary::GitRemote
            | FailureBoundary::GitHubAPI
            | FailureBoundary::ModelProvider
            | FailureBoundary::AgentProcess
    )
}

fn is_internal_deterministic_boundary(boundary: FailureBoundary) -> bool {
    matches!(
        boundary,
        FailureBoundary::GitLocal
            | FailureBoundary::Storage
            | FailureBoundary::Config
            | FailureBoundary::Checkpoint
            | FailureBoundary::Policy
    )
}

// ---------------------------------------------------------------------------
// Message pattern helpers
// ---------------------------------------------------------------------------

fn is_manual_worktree_message(msg: &str) -> bool {
    msg.contains("worktree is dirty")
        || msg.contains("uncommitted changes")
        || msg.contains("local changes")
        || msg.contains("merge conflict")
        || msg.contains("dirty worktree")
        || msg.contains("manual intervention required")
}

fn is_gql_unauthorized(msg: &str) -> bool {
    msg.contains("401")
        || msg.contains("unauthorized")
        || msg.contains("not authorized")
        || msg.contains("authentication failed")
        || msg.contains("bad credentials")
}

fn is_deterministic_denial(msg: &str) -> bool {
    msg.contains("not found")
        || msg.contains("does not exist")
        || msg.contains("access denied")
        || msg.contains("forbidden")
        || msg.contains("403")
        || msg.contains("404")
        || msg.contains("repository access blocked")
        || msg.contains("no such repository")
}

fn is_http_4xx_denial(msg: &str) -> bool {
    msg.contains("400")
        || msg.contains("422")
        || msg.contains("validation failed")
        || msg.contains("unprocessable entity")
}

// ---------------------------------------------------------------------------
// Retry logic
// ---------------------------------------------------------------------------

/// Determine whether a queue item should be retried given its failure kind.
pub fn should_retry_queue_item(item: &QueueItemRecord, kind: &QueueFailureKind) -> bool {
    match kind {
        QueueFailureKind::RetryableTransient => true,
        QueueFailureKind::RetryableAfterResume => true,
        QueueFailureKind::NonRetryable => item.max_attempts < 0 || item.attempts < item.max_attempts,
        QueueFailureKind::ManualIntervention => false,
    }
}

/// Compute exponential backoff delay: `base_delay * 2^attempt`, capped at 300s.
pub fn compute_retry_delay(attempt: u32, base_delay_ms: u64) -> Duration {
    let delay_ms = (base_delay_ms * 2u64.pow(attempt)).min(300_000);
    Duration::from_millis(delay_ms)
}

// ---------------------------------------------------------------------------
// Runner step boundary mapping
// ---------------------------------------------------------------------------

/// Return the expected failure boundary for a (runner_kind, step) pair.
pub fn step_boundary(runner: &str, step: &str) -> FailureBoundary {
    match (runner, step) {
        // Planner steps
        ("planner", "discover-issues") => FailureBoundary::GitHubAPI,
        ("planner", "prepare-worktree") => FailureBoundary::GitRemote,
        ("planner", "write-spec") => FailureBoundary::ModelProvider,
        ("planner", "publish") => FailureBoundary::GitHubAPI,
        ("planner", "notify") => FailureBoundary::GitHubAPI,

        // Reviewer steps
        ("reviewer", "review-pr") => FailureBoundary::GitHubAPI,
        ("reviewer", "check-worktree") => FailureBoundary::GitLocal,
        ("reviewer", "submit-review") => FailureBoundary::GitHubAPI,
        ("reviewer", "update-pr") => FailureBoundary::GitHubAPI,

        // Fixer steps
        ("fixer", "fix-pr") => FailureBoundary::GitHubAPI,
        ("fixer", "prepare-worktree") => FailureBoundary::GitRemote,
        ("fixer", "commit-fix") => FailureBoundary::GitLocal,
        ("fixer", "push-fix") => FailureBoundary::GitRemote,

        // Worker steps
        ("worker", "process-issue") => FailureBoundary::GitHubAPI,
        ("worker", "create-worktree") => FailureBoundary::GitRemote,
        ("worker", "implement") => FailureBoundary::AgentProcess,
        ("worker", "commit-push") => FailureBoundary::GitRemote,
        ("worker", "open-pr") => FailureBoundary::GitHubAPI,

        _ => FailureBoundary::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_external_boundary() {
        let err = SchedulerError::Other("network error".into());
        let ctx = FailureClassificationContext {
            runner: "planner".into(),
            step: "prepare-worktree".into(),
            boundary: FailureBoundary::GitRemote,
            side_effect_state: None,
        };
        assert_eq!(classify_failure(&err, &ctx), QueueFailureKind::RetryableTransient);
    }

    #[test]
    fn test_classify_internal_boundary() {
        let err = SchedulerError::Other("config error".into());
        let ctx = FailureClassificationContext {
            runner: "planner".into(),
            step: "discover-issues".into(),
            boundary: FailureBoundary::Config,
            side_effect_state: None,
        };
        assert_eq!(classify_failure(&err, &ctx), QueueFailureKind::NonRetryable);
    }

    #[test]
    fn test_classify_manual_worktree() {
        let err = SchedulerError::Other("worktree is dirty".into());
        let ctx = FailureClassificationContext {
            runner: "fixer".into(),
            step: "commit-fix".into(),
            boundary: FailureBoundary::GitLocal,
            side_effect_state: None,
        };
        assert_eq!(classify_failure(&err, &ctx), QueueFailureKind::ManualIntervention);
    }

    #[test]
    fn test_classify_github_400() {
        let err = SchedulerError::Other("HTTP 400 validation failed".into());
        let ctx = FailureClassificationContext {
            runner: "reviewer".into(),
            step: "submit-review".into(),
            boundary: FailureBoundary::GitHubAPI,
            side_effect_state: None,
        };
        assert_eq!(classify_failure(&err, &ctx), QueueFailureKind::NonRetryable);
    }

    #[test]
    fn test_should_retry_retryable() {
        let item = QueueItemRecord { max_attempts: 3, attempts: 1, ..create_test_queue_item() };
        assert!(should_retry_queue_item(&item, &QueueFailureKind::RetryableTransient));
        assert!(should_retry_queue_item(&item, &QueueFailureKind::RetryableAfterResume));
    }

    #[test]
    fn test_should_retry_non_retryable_under_max() {
        let item = QueueItemRecord { max_attempts: 3, attempts: 1, ..create_test_queue_item() };
        assert!(should_retry_queue_item(&item, &QueueFailureKind::NonRetryable));
    }

    #[test]
    fn test_should_retry_non_retryable_at_max() {
        let item = QueueItemRecord { max_attempts: 3, attempts: 3, ..create_test_queue_item() };
        assert!(!should_retry_queue_item(&item, &QueueFailureKind::NonRetryable));
    }

    #[test]
    fn test_should_not_retry_manual() {
        let item = create_test_queue_item();
        assert!(!should_retry_queue_item(&item, &QueueFailureKind::ManualIntervention));
    }

    #[test]
    fn test_compute_retry_delay_exponential() {
        let delay = compute_retry_delay(0, 1000);
        assert_eq!(delay.as_millis(), 1000);

        let delay = compute_retry_delay(1, 1000);
        assert_eq!(delay.as_millis(), 2000);

        let delay = compute_retry_delay(2, 1000);
        assert_eq!(delay.as_millis(), 4000);
    }

    #[test]
    fn test_compute_retry_delay_capped() {
        // 1000 * 2^9 = 512000, capped at 300000
        let delay = compute_retry_delay(9, 1000);
        assert_eq!(delay.as_millis(), 300_000);
    }

    #[test]
    fn test_step_boundary_planner() {
        assert_eq!(step_boundary("planner", "discover-issues"), FailureBoundary::GitHubAPI);
        assert_eq!(step_boundary("planner", "prepare-worktree"), FailureBoundary::GitRemote);
        assert_eq!(step_boundary("planner", "write-spec"), FailureBoundary::ModelProvider);
    }

    #[test]
    fn test_step_boundary_reviewer() {
        assert_eq!(step_boundary("reviewer", "review-pr"), FailureBoundary::GitHubAPI);
        assert_eq!(step_boundary("reviewer", "check-worktree"), FailureBoundary::GitLocal);
    }

    #[test]
    fn test_step_boundary_unknown() {
        assert_eq!(step_boundary("planner", "unknown-step"), FailureBoundary::Unknown);
    }

    #[test]
    fn test_is_external_boundary() {
        assert!(is_external_boundary(FailureBoundary::GitRemote));
        assert!(is_external_boundary(FailureBoundary::GitHubAPI));
        assert!(is_external_boundary(FailureBoundary::ModelProvider));
        assert!(!is_external_boundary(FailureBoundary::GitLocal));
    }

    fn create_test_queue_item() -> QueueItemRecord {
        QueueItemRecord {
            id: "test-id".into(),
            project_id: None,
            loop_id: None,
            r#type: "planner".into(),
            target_type: "issue".into(),
            target_id: "123".into(),
            repo: None,
            pr_number: None,
            dedupe_key: "test-dedup".into(),
            priority: 1,
            status: "queued".into(),
            available_at: "2026-06-22T00:00:00.000Z".into(),
            attempts: 0,
            max_attempts: 3,
            claimed_by: None,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            lock_key: None,
            payload_json: None,
            last_error: None,
            last_error_kind: None,
            created_at: "2026-06-22T00:00:00.000Z".into(),
            updated_at: "2026-06-22T00:00:00.000Z".into(),
        }
    }
}
