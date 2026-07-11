//! Reviewer → Fixer handoff policy.
//!
//! Pure decision logic for when a completed review should enqueue a fixer
//! queue item, plus helpers to create/reuse a fixer loop and deduped queue
//! entry.

use chrono::Utc;
use uuid::Uuid;

use looper_storage::record::{LoopRecord, QueueItemRecord};
use looper_storage::repos::Repositories;

// ---------------------------------------------------------------------------
// Pure policy
// ---------------------------------------------------------------------------

/// Inputs for the reviewer→fixer handoff decision (no I/O).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixerEnqueueContext {
    /// True when criteria verification found failures or unverifiable items.
    pub has_criteria_issues: bool,
    /// GitHub `reviewDecision` string (`APPROVED`, `CHANGES_REQUESTED`, …).
    pub review_decision: Option<String>,
    /// True when any required / blocking CI checks are failing.
    pub has_failing_required_checks: bool,
}

/// Result of [`should_enqueue_fixer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueDecision {
    /// Enqueue a fixer item; `reason` is a stable machine-readable token.
    Enqueue { reason: &'static str },
    /// Do not enqueue; `reason` explains why (e.g. clean approve).
    Skip { reason: &'static str },
}

impl EnqueueDecision {
    /// Whether a fixer queue item should be created / reclaimed.
    pub fn should_enqueue(&self) -> bool {
        matches!(self, Self::Enqueue { .. })
    }

    /// Machine-readable reason string for logging.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Enqueue { reason } | Self::Skip { reason } => reason,
        }
    }
}

/// Decide whether the reviewer publish path should enqueue a fixer item.
///
/// Enqueue if **any** of:
/// - criteria / has_issues
/// - `CHANGES_REQUESTED` review decision
/// - failing required checks
///
/// Skip on a clean APPROVE (and any other state with none of the triggers).
pub fn should_enqueue_fixer(ctx: &FixerEnqueueContext) -> EnqueueDecision {
    if ctx.has_criteria_issues {
        return EnqueueDecision::Enqueue { reason: "criteria_issues" };
    }
    if ctx.review_decision.as_deref() == Some("CHANGES_REQUESTED") {
        return EnqueueDecision::Enqueue { reason: "changes_requested" };
    }
    if ctx.has_failing_required_checks {
        return EnqueueDecision::Enqueue { reason: "failing_required_checks" };
    }
    EnqueueDecision::Skip { reason: "clean_review" }
}

/// Canonical dedupe key for fixer queue items: `fixer-{project_id}-pr-{pr_number}`.
pub fn fixer_dedupe_key(project_id: &str, pr_number: i64) -> String {
    format!("fixer-{project_id}-pr-{pr_number}")
}

// ---------------------------------------------------------------------------
// Storage helper — create/reuse fixer loop + deduped queue item
// ---------------------------------------------------------------------------

/// Outcome of ensuring a fixer queue item exists.
#[derive(Debug, Clone)]
pub struct FixerEnqueueResult {
    pub queue_item: QueueItemRecord,
    /// True when a new queue row was inserted (false = existing active item).
    pub is_new: bool,
    pub loop_id: String,
    pub dedupe_key: String,
}

const TERMINAL_LOOP_STATUSES: &[&str] = &["completed", "failed", "cancelled", "terminated"];

/// Prefer an active fixer loop for the PR; otherwise create one. Then
/// `create_or_get_active_by_dedupe` a fixer queue item.
///
/// Does **not** touch or destroy any reviewer loop.
pub fn ensure_fixer_queue_item(
    repos: &Repositories,
    project_id: &str,
    repo: Option<&str>,
    pr_number: i64,
    reason: &str,
) -> Result<FixerEnqueueResult, String> {
    let now_iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let dedupe_key = fixer_dedupe_key(project_id, pr_number);

    // 1. Prefer existing active fixer loop for this project + PR.
    let all_loops = repos.loops.list().map_err(|e| e.to_string())?;
    let existing = all_loops.into_iter().find(|l| {
        l.project_id == project_id
            && l.r#type == "fixer"
            && l.pr_number == Some(pr_number)
            && !TERMINAL_LOOP_STATUSES.contains(&l.status.as_str())
    });

    let loop_id = if let Some(l) = existing {
        l.id
    } else {
        // 2. Create fixer loop (type=fixer, target=pr).
        let lid = Uuid::new_v4().to_string();
        let seq = repos.loops.allocate_seq().map_err(|e| e.to_string())?;
        let rec = LoopRecord {
            id: lid.clone(),
            seq,
            project_id: project_id.to_string(),
            r#type: "fixer".into(),
            target_type: "pull_request".into(),
            target_id: Some(pr_number.to_string()),
            repo: repo.map(|s| s.to_string()),
            pr_number: Some(pr_number),
            status: "queued".into(),
            config_json: None,
            metadata_json: Some(
                serde_json::json!({
                    "enqueued_from": "reviewer_publish",
                    "reason": reason,
                })
                .to_string(),
            ),
            last_run_at: None,
            next_run_at: None,
            created_at: now_iso.clone(),
            updated_at: now_iso.clone(),
        };
        repos.loops.upsert(&rec).map_err(|e| e.to_string())?;
        lid
    };

    // 3. Deduped queue item.
    let record = QueueItemRecord {
        id: Uuid::new_v4().to_string(),
        project_id: Some(project_id.to_string()),
        loop_id: Some(loop_id.clone()),
        r#type: "fixer".into(),
        target_type: "pull_request".into(),
        target_id: pr_number.to_string(),
        repo: repo.map(|s| s.to_string()),
        pr_number: Some(pr_number),
        dedupe_key: dedupe_key.clone(),
        priority: 2,
        status: "queued".into(),
        available_at: now_iso.clone(),
        attempts: 0,
        max_attempts: 3,
        claimed_by: None,
        claimed_at: None,
        started_at: None,
        finished_at: None,
        lock_key: None,
        payload_json: Some(
            serde_json::json!({
                "enqueued_from": "reviewer_publish",
                "reason": reason,
            })
            .to_string(),
        ),
        last_error: None,
        last_error_kind: None,
        created_at: now_iso.clone(),
        updated_at: now_iso,
    };

    let (queue_item, is_new) = repos.queue.create_or_get_active_by_dedupe(&record).map_err(|e| e.to_string())?;

    Ok(FixerEnqueueResult { queue_item, is_new, loop_id, dedupe_key })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use looper_storage::migration::run_migrations;
    use looper_storage::record::ProjectRecord;
    use rusqlite::Connection;

    fn sample_ctx(
        has_criteria_issues: bool,
        review_decision: Option<&str>,
        has_failing_required_checks: bool,
    ) -> FixerEnqueueContext {
        FixerEnqueueContext {
            has_criteria_issues,
            review_decision: review_decision.map(|s| s.to_string()),
            has_failing_required_checks,
        }
    }

    #[test]
    fn should_enqueue_fixer_table() {
        // (criteria, decision, failing_checks) → should_enqueue, reason
        let cases: &[((bool, Option<&str>, bool), bool, &str)] = &[
            // clean APPROVE → no
            ((false, Some("APPROVED"), false), false, "clean_review"),
            // no signals at all → no
            ((false, None, false), false, "clean_review"),
            ((false, Some("REVIEW_REQUIRED"), false), false, "clean_review"),
            // criteria fail → yes
            ((true, Some("APPROVED"), false), true, "criteria_issues"),
            ((true, None, false), true, "criteria_issues"),
            // CHANGES_REQUESTED → yes
            ((false, Some("CHANGES_REQUESTED"), false), true, "changes_requested"),
            // failing checks → yes
            ((false, Some("APPROVED"), true), true, "failing_required_checks"),
            ((false, None, true), true, "failing_required_checks"),
            // multiple triggers: criteria wins first
            ((true, Some("CHANGES_REQUESTED"), true), true, "criteria_issues"),
            // CR + failing checks (no criteria)
            ((false, Some("CHANGES_REQUESTED"), true), true, "changes_requested"),
        ];

        for (i, ((criteria, decision, failing), want_yes, want_reason)) in cases.iter().enumerate() {
            let d = should_enqueue_fixer(&sample_ctx(*criteria, *decision, *failing));
            assert_eq!(
                d.should_enqueue(),
                *want_yes,
                "case {i}: criteria={criteria} decision={decision:?} failing={failing}"
            );
            assert_eq!(
                d.reason(),
                *want_reason,
                "case {i}: criteria={criteria} decision={decision:?} failing={failing}"
            );
        }
    }

    #[test]
    fn fixer_dedupe_key_format() {
        assert_eq!(fixer_dedupe_key("proj-abc", 42), "fixer-proj-abc-pr-42");
        assert_eq!(fixer_dedupe_key("p1", 7), "fixer-p1-pr-7");
    }

    fn setup_repos() -> Repositories {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        run_migrations(&mut conn).unwrap();
        Repositories::new(conn)
    }

    fn seed_project(repos: &Repositories, id: &str) {
        let t = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        repos
            .projects
            .upsert(&ProjectRecord {
                id: id.into(),
                name: "test".into(),
                repo_path: "/tmp/r".into(),
                base_branch: Some("main".into()),
                archived: false,
                metadata_json: None,
                created_at: t.clone(),
                updated_at: t,
            })
            .unwrap();
    }

    #[test]
    fn ensure_fixer_queue_item_inserts_then_dedupes() {
        let repos = setup_repos();
        seed_project(&repos, "proj-1");

        let first = ensure_fixer_queue_item(&repos, "proj-1", Some("owner/repo"), 99, "criteria_issues").unwrap();
        assert!(first.is_new);
        assert_eq!(first.dedupe_key, "fixer-proj-1-pr-99");
        assert_eq!(first.queue_item.r#type, "fixer");
        assert_eq!(first.queue_item.pr_number, Some(99));
        assert_eq!(first.queue_item.max_attempts, 3);
        assert_eq!(first.queue_item.status, "queued");

        // Loop was created as fixer / pull_request.
        let loop_rec = repos.loops.get_by_id(&first.loop_id).unwrap().unwrap();
        assert_eq!(loop_rec.r#type, "fixer");
        assert_eq!(loop_rec.pr_number, Some(99));

        // Second call: same dedupe key → not new; same loop preferred.
        let second = ensure_fixer_queue_item(&repos, "proj-1", Some("owner/repo"), 99, "criteria_issues").unwrap();
        assert!(!second.is_new);
        assert_eq!(second.queue_item.id, first.queue_item.id);
        assert_eq!(second.loop_id, first.loop_id);
        assert_eq!(second.dedupe_key, first.dedupe_key);
    }

    #[test]
    fn ensure_fixer_reuses_existing_active_fixer_loop() {
        let repos = setup_repos();
        seed_project(&repos, "proj-2");
        let t = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        // Pre-create an active fixer loop for PR #5.
        let existing_id = "loop-fixer-existing".to_string();
        repos
            .loops
            .upsert(&LoopRecord {
                id: existing_id.clone(),
                seq: 1,
                project_id: "proj-2".into(),
                r#type: "fixer".into(),
                target_type: "pull_request".into(),
                target_id: Some("5".into()),
                repo: Some("o/r".into()),
                pr_number: Some(5),
                status: "active".into(),
                config_json: None,
                metadata_json: None,
                last_run_at: None,
                next_run_at: None,
                created_at: t.clone(),
                updated_at: t,
            })
            .unwrap();

        let result = ensure_fixer_queue_item(&repos, "proj-2", Some("o/r"), 5, "changes_requested").unwrap();
        assert_eq!(result.loop_id, existing_id);
        assert!(result.is_new);
    }
}
