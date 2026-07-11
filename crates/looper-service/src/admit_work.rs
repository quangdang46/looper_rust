//! Admit work: create/reuse a loop + claimable queue item for a role target.
//!
//! This is the service-layer golden path for operators and (later) API/CLI
//! wrappers. It does **not** trigger HTTP or scheduler ticks — that is B2.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::info;

use looper_storage::{
    eventlog::new_event_id,
    record::{LoopRecord, QueueItemRecord},
    repos::Repositories,
};
use looper_types::{loop_target_key, LoopStatus, LoopTarget, LoopTargetType, LoopType};

use crate::error::{Result, ServiceError};
use crate::project_service::{normalize_repo_spec, resolve_project_repo};

/// Input for [`AdmitWorkService::admit_work`].
#[derive(Debug, Clone)]
pub struct AdmitWorkInput {
    pub project_id: String,
    /// Role name: `planner` | `reviewer` | `worker` | `fixer`.
    pub role: String,
    pub issue_number: Option<i64>,
    pub pr_number: Option<i64>,
    /// Optional explicit `owner/name` (or github URL). When absent, resolved
    /// from project metadata / repo_path via [`resolve_project_repo`].
    pub repo: Option<String>,
    pub priority: Option<i64>,
    /// Optional JSON object merged into loop metadata and queue payload.
    pub metadata: Option<serde_json::Value>,
}

/// Result of admitting work.
#[derive(Debug, Clone)]
pub struct AdmitWorkResult {
    pub loop_record: LoopRecord,
    pub queue_item: QueueItemRecord,
    /// True when a new loop row was inserted for this call.
    pub created_new_loop: bool,
}

/// Service that admits role work into the scheduler queue.
pub struct AdmitWorkService {
    repos: Arc<Repositories>,
    now: Box<dyn Fn() -> DateTime<Utc>>,
}

impl AdmitWorkService {
    pub fn new<F>(repos: Arc<Repositories>, now: F) -> Self
    where
        F: Fn() -> DateTime<Utc> + 'static,
    {
        Self { repos, now: Box::new(now) }
    }

    /// Create or reuse an active loop and ensure a claimable queue item exists.
    ///
    /// Behavior:
    /// 1. Load project; resolve repo (explicit input or B8 `resolve_project_repo`).
    /// 2. Validate role + target (issue vs PR table).
    /// 3. Create/reuse loop (active conflict = reuse).
    /// 4. Queue item `type=role`, `status=queued`, dedupe via
    ///    `create_or_get_active_by_dedupe`.
    pub fn admit_work(&self, input: AdmitWorkInput) -> Result<AdmitWorkResult> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        // 1. Parse role
        let role = parse_role(&input.role)?;

        // 2. Load project
        let project = self
            .repos
            .projects
            .get_by_id(&input.project_id)?
            .ok_or_else(|| ServiceError::ProjectNotFound(input.project_id.clone()))?;

        // 3. Resolve repo (explicit override, else B8 contract)
        let repo = if let Some(ref r) = input.repo {
            let trimmed = r.trim();
            if trimmed.is_empty() {
                resolve_project_repo(&project)?
            } else {
                normalize_repo_spec(trimmed)
            }
        } else {
            resolve_project_repo(&project)?
        };

        // 4. Validate role + target → (LoopTarget, target_id for queue, dedupe suffix)
        let (loop_target, queue_target_type, queue_target_id, pr_number, dedupe_key) =
            resolve_role_target(role, &input.project_id, &repo, input.issue_number, input.pr_number)?;

        let priority = input.priority.unwrap_or_else(|| default_priority(role));

        // 5. Create or reuse active loop
        let target_key = loop_target_key(&loop_target);
        let (loop_record, created_new_loop) = self.create_or_reuse_loop(
            &input.project_id,
            role,
            &loop_target,
            &target_key,
            &repo,
            pr_number,
            input.metadata.as_ref(),
            &now_iso,
        )?;

        // 6. Build queue item + create_or_get_active_by_dedupe
        let payload = build_payload(role, pr_number, input.issue_number, input.metadata.as_ref());

        let candidate = QueueItemRecord {
            id: new_event_id("q"),
            project_id: Some(input.project_id.clone()),
            loop_id: Some(loop_record.id.clone()),
            r#type: role.as_str().to_string(),
            target_type: queue_target_type.to_string(),
            target_id: queue_target_id,
            repo: Some(repo.clone()),
            pr_number,
            dedupe_key: dedupe_key.clone(),
            priority,
            status: "queued".into(),
            available_at: now_iso.clone(),
            attempts: 0,
            max_attempts: 3,
            claimed_by: None,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            lock_key: Some(dedupe_key.clone()),
            payload_json: Some(payload.to_string()),
            last_error: None,
            last_error_kind: None,
            created_at: now_iso.clone(),
            updated_at: now_iso,
        };

        let (queue_item, _is_new_queue) = self.repos.queue.create_or_get_active_by_dedupe(&candidate)?;

        info!(
            project_id = %input.project_id,
            role = %role.as_str(),
            loop_id = %loop_record.id,
            queue_id = %queue_item.id,
            dedupe_key = %dedupe_key,
            created_new_loop,
            "admit_work"
        );

        Ok(AdmitWorkResult { loop_record, queue_item, created_new_loop })
    }

    /// Find an active loop for (project, type, target) or create one.
    ///
    /// Mirrors [`crate::LoopService`] conflict rules: one active loop per
    /// `(project_id, type, target_key)`. When an active match exists (including
    /// discovery-style bare `target_id`), reuse it instead of erroring.
    #[allow(clippy::too_many_arguments)]
    fn create_or_reuse_loop(
        &self,
        project_id: &str,
        role: LoopType,
        target: &LoopTarget,
        target_key: &str,
        repo: &str,
        pr_number: Option<i64>,
        metadata: Option<&serde_json::Value>,
        now_iso: &str,
    ) -> Result<(LoopRecord, bool)> {
        let all_loops = self.repos.loops.list()?;
        let role_str = role.as_str();

        if let Some(existing) = all_loops.iter().find(|l| {
            l.project_id == project_id
                && l.r#type == role_str
                && is_active_loop_status(&l.status)
                && loop_matches_target(l, target_key, pr_number, target)
        }) {
            return Ok((existing.clone(), false));
        }

        // LoopService-compatible conflict guard (same project/type/target_key).
        let conflict = all_loops.iter().any(|l| {
            l.project_id == project_id
                && l.r#type == role_str
                && l.target_id.as_deref() == Some(target_key)
                && is_active_loop_status(&l.status)
        });
        if conflict {
            return Err(ServiceError::ActiveLoopConflict {
                project_id: project_id.to_string(),
                loop_type: role_str.to_string(),
                target_key: target_key.to_string(),
            });
        }

        if !role.supports_target_type(target.target_type) {
            return Err(ServiceError::Other(format!(
                "loop type {} cannot target {}",
                role.as_str(),
                target.target_type.as_str(),
            )));
        }

        let metadata_json = metadata
            .map(|m| {
                let mut obj = m.clone();
                if let serde_json::Value::Object(ref mut map) = obj {
                    map.insert("admitted_via".into(), serde_json::json!("admit_work"));
                    map.insert("repo".into(), serde_json::json!(repo));
                }
                obj.to_string()
            })
            .or_else(|| {
                Some(
                    serde_json::json!({
                        "admitted_via": "admit_work",
                        "repo": repo,
                    })
                    .to_string(),
                )
            });

        let seq = self.repos.loops.allocate_seq()?;
        let record = LoopRecord {
            id: new_event_id("loop"),
            seq,
            project_id: project_id.to_string(),
            r#type: role_str.to_string(),
            target_type: target.target_type.as_str().to_string(),
            target_id: Some(target_key.to_string()),
            repo: Some(repo.to_string()),
            // Only PR-targeted roles set loop.pr_number; issue targets leave it None.
            pr_number,
            status: LoopStatus::Queued.as_str().to_string(),
            config_json: None,
            metadata_json,
            last_run_at: None,
            next_run_at: Some(now_iso.to_string()),
            created_at: now_iso.to_string(),
            updated_at: now_iso.to_string(),
        };

        self.repos.loops.upsert(&record)?;
        info!(loop_id = %record.id, seq = %record.seq, role = %role_str, "admit_work loop created");

        Ok((record, true))
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn parse_role(role: &str) -> Result<LoopType> {
    let trimmed = role.trim();
    if trimmed.is_empty() {
        return Err(ServiceError::Other("invalid role: empty (expected planner|reviewer|worker|fixer)".into()));
    }
    trimmed
        .parse::<LoopType>()
        .map_err(|_| ServiceError::Other(format!("invalid role '{trimmed}' (expected planner|reviewer|worker|fixer)")))
}

/// Role → required target validation and dedupe key construction.
///
/// | Role     | Required target                          |
/// |----------|------------------------------------------|
/// | planner  | `issue_number`                           |
/// | reviewer | `pr_number`                              |
/// | fixer    | `pr_number`                              |
/// | worker   | `issue_number` **or** `pr_number`        |
///
/// Dedupe: `{role}-{project}-issue|{pr}-{n}`
fn resolve_role_target(
    role: LoopType,
    project_id: &str,
    repo: &str,
    issue_number: Option<i64>,
    pr_number: Option<i64>,
) -> Result<(LoopTarget, &'static str, String, Option<i64>, String)> {
    match role {
        LoopType::Planner => {
            let n = require_positive("issue_number", issue_number, "planner requires issue_number")?;
            let target = LoopTarget {
                target_type: LoopTargetType::Issue,
                project_id: None,
                repo: Some(repo.to_string()),
                number: Some(n),
            };
            let dedupe = format!("planner-{project_id}-issue-{n}");
            Ok((target, "issue", n.to_string(), None, dedupe))
        }
        LoopType::Reviewer => {
            let n = require_positive("pr_number", pr_number, "reviewer requires pr_number")?;
            let target = LoopTarget {
                target_type: LoopTargetType::PullRequest,
                project_id: None,
                repo: Some(repo.to_string()),
                number: Some(n),
            };
            let dedupe = format!("reviewer-{project_id}-pr-{n}");
            Ok((target, "pull_request", n.to_string(), Some(n), dedupe))
        }
        LoopType::Fixer => {
            let n = require_positive("pr_number", pr_number, "fixer requires pr_number")?;
            let target = LoopTarget {
                target_type: LoopTargetType::PullRequest,
                project_id: None,
                repo: Some(repo.to_string()),
                number: Some(n),
            };
            let dedupe = format!("fixer-{project_id}-pr-{n}");
            Ok((target, "pull_request", n.to_string(), Some(n), dedupe))
        }
        LoopType::Worker => {
            // Prefer PR when both provided (implementation often tracks a PR).
            if let Some(n) = pr_number {
                let n = require_positive("pr_number", Some(n), "worker pr_number must be positive")?;
                let target = LoopTarget {
                    target_type: LoopTargetType::PullRequest,
                    project_id: None,
                    repo: Some(repo.to_string()),
                    number: Some(n),
                };
                let dedupe = format!("worker-{project_id}-pr-{n}");
                Ok((target, "pull_request", n.to_string(), Some(n), dedupe))
            } else if let Some(n) = issue_number {
                let n = require_positive("issue_number", Some(n), "worker issue_number must be positive")?;
                let target = LoopTarget {
                    target_type: LoopTargetType::Issue,
                    project_id: None,
                    repo: Some(repo.to_string()),
                    number: Some(n),
                };
                let dedupe = format!("worker-{project_id}-issue-{n}");
                Ok((target, "issue", n.to_string(), None, dedupe))
            } else {
                Err(ServiceError::Other("worker requires issue_number or pr_number".into()))
            }
        }
    }
}

fn require_positive(field: &str, value: Option<i64>, missing_msg: &str) -> Result<i64> {
    match value {
        None => Err(ServiceError::Other(missing_msg.into())),
        Some(n) if n <= 0 => Err(ServiceError::Other(format!("{field} must be a positive integer, got {n}"))),
        Some(n) => Ok(n),
    }
}

fn default_priority(role: LoopType) -> i64 {
    match role {
        LoopType::Planner => 10,
        LoopType::Reviewer => 1,
        LoopType::Worker => 50,
        LoopType::Fixer => 2,
    }
}

fn is_active_loop_status(status: &str) -> bool {
    matches!(status, "idle" | "queued" | "running" | "paused" | "waiting" | "active")
}

fn loop_matches_target(l: &LoopRecord, target_key: &str, pr_number: Option<i64>, target: &LoopTarget) -> bool {
    if l.target_id.as_deref() == Some(target_key) {
        return true;
    }
    if let Some(n) = pr_number {
        if l.pr_number == Some(n) {
            return true;
        }
    }
    // Discovery paths often store bare issue/PR number in target_id.
    if let Some(n) = target.number {
        if l.target_id.as_deref() == Some(&n.to_string()) {
            return true;
        }
    }
    false
}

fn build_payload(
    role: LoopType,
    pr_number: Option<i64>,
    issue_number: Option<i64>,
    metadata: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("admitted_via".into(), serde_json::json!("admit_work"));
    map.insert("role".into(), serde_json::json!(role.as_str()));
    if let Some(n) = pr_number {
        map.insert("pr_number".into(), serde_json::json!(n));
    }
    if let Some(n) = issue_number {
        map.insert("issue_number".into(), serde_json::json!(n));
    }
    if let Some(serde_json::Value::Object(extra)) = metadata {
        for (k, v) in extra {
            map.entry(k.clone()).or_insert_with(|| v.clone());
        }
    } else if let Some(other) = metadata {
        map.insert("metadata".into(), other.clone());
    }
    serde_json::Value::Object(map)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use looper_storage::migration::run_migrations;
    use looper_storage::record::ProjectRecord;
    use rusqlite::Connection;

    fn setup() -> Arc<Repositories> {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;").unwrap();
        run_migrations(&mut conn).unwrap();
        Arc::new(Repositories::new(conn))
    }

    fn seed_project(repos: &Repositories, id: &str, repo: Option<&str>) -> ProjectRecord {
        let metadata_json = repo.map(|r| serde_json::json!({ "repo": r }).to_string());
        let p = ProjectRecord {
            id: id.into(),
            name: id.into(),
            repo_path: "/tmp/checkout".into(),
            metadata_json,
            base_branch: Some("main".into()),
            archived: false,
            created_at: "2024-01-01T00:00:00.000Z".into(),
            updated_at: "2024-01-01T00:00:00.000Z".into(),
        };
        repos.projects.upsert(&p).unwrap();
        p
    }

    fn svc(repos: Arc<Repositories>) -> AdmitWorkService {
        AdmitWorkService::new(repos, Utc::now)
    }

    #[test]
    fn admit_work_happy_planner() {
        let repos = setup();
        seed_project(&repos, "proj-1", Some("acme/widget"));
        let s = svc(repos.clone());

        let result = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "planner".into(),
                issue_number: Some(12),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap();

        assert!(result.created_new_loop);
        assert_eq!(result.loop_record.r#type, "planner");
        assert_eq!(result.loop_record.target_type, "issue");
        assert_eq!(result.loop_record.repo.as_deref(), Some("acme/widget"));
        assert_eq!(result.loop_record.status, "queued");

        assert_eq!(result.queue_item.r#type, "planner");
        assert_eq!(result.queue_item.status, "queued");
        assert_eq!(result.queue_item.dedupe_key, "planner-proj-1-issue-12");
        assert_eq!(result.queue_item.target_type, "issue");
        assert_eq!(result.queue_item.target_id, "12");
        assert_eq!(result.queue_item.repo.as_deref(), Some("acme/widget"));
        assert_eq!(result.queue_item.loop_id.as_deref(), Some(result.loop_record.id.as_str()));
        assert_eq!(result.queue_item.priority, 10);

        // Claim processors must see a queued row.
        let listed = repos.queue.list_by_statuses(&["queued".into()]).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, result.queue_item.id);
    }

    #[test]
    fn admit_work_idempotent_re_admit() {
        let repos = setup();
        seed_project(&repos, "proj-1", Some("acme/widget"));
        let s = svc(repos.clone());

        let first = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "planner".into(),
                issue_number: Some(12),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap();

        let second = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "planner".into(),
                issue_number: Some(12),
                pr_number: None,
                repo: None,
                priority: Some(99),
                metadata: None,
            })
            .unwrap();

        assert!(!second.created_new_loop);
        assert_eq!(first.loop_record.id, second.loop_record.id);
        assert_eq!(first.queue_item.id, second.queue_item.id);
        assert_eq!(second.queue_item.dedupe_key, "planner-proj-1-issue-12");

        let all = repos.queue.list().unwrap();
        assert_eq!(all.len(), 1, "must not create duplicate queue rows");
    }

    #[test]
    fn admit_work_missing_repo_errors() {
        let repos = setup();
        seed_project(&repos, "proj-bare", None);
        let s = svc(repos);

        let err = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-bare".into(),
                role: "planner".into(),
                issue_number: Some(1),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap_err();

        match err {
            ServiceError::ProjectRepoUnresolved(msg) => {
                assert!(msg.contains("proj-bare"), "message should name project: {msg}");
                assert!(msg.contains("owner/name") || msg.contains("repo"), "message should be actionable: {msg}");
            }
            other => panic!("expected ProjectRepoUnresolved, got {other:?}"),
        }
    }

    #[test]
    fn admit_work_explicit_repo_override() {
        let repos = setup();
        seed_project(&repos, "proj-1", None); // no metadata.repo
        let s = svc(repos);

        let result = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "planner".into(),
                issue_number: Some(3),
                pr_number: None,
                repo: Some("https://github.com/acme/explicit.git".into()),
                priority: None,
                metadata: None,
            })
            .unwrap();

        assert_eq!(result.queue_item.repo.as_deref(), Some("acme/explicit"));
        assert_eq!(result.loop_record.repo.as_deref(), Some("acme/explicit"));
    }

    #[test]
    fn admit_work_missing_target_planner() {
        let repos = setup();
        seed_project(&repos, "proj-1", Some("acme/widget"));
        let s = svc(repos);

        let err = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "planner".into(),
                issue_number: None,
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("issue_number"), "got: {msg}");
    }

    #[test]
    fn admit_work_missing_pr_for_reviewer() {
        let repos = setup();
        seed_project(&repos, "proj-1", Some("acme/widget"));
        let s = svc(repos);

        let err = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "reviewer".into(),
                issue_number: Some(1),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("pr_number"), "got: {msg}");
    }

    #[test]
    fn admit_work_invalid_role() {
        let repos = setup();
        seed_project(&repos, "proj-1", Some("acme/widget"));
        let s = svc(repos);

        let err = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "coordinator".into(),
                issue_number: Some(1),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("invalid role"), "got: {msg}");
    }

    #[test]
    fn admit_work_project_not_found() {
        let repos = setup();
        let s = svc(repos);

        let err = s
            .admit_work(AdmitWorkInput {
                project_id: "nope".into(),
                role: "planner".into(),
                issue_number: Some(1),
                pr_number: None,
                repo: Some("acme/widget".into()),
                priority: None,
                metadata: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::ProjectNotFound(_)));
    }

    #[test]
    fn admit_work_reviewer_and_fixer_happy() {
        let repos = setup();
        seed_project(&repos, "proj-1", Some("acme/widget"));
        let s = svc(repos.clone());

        let rev = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "reviewer".into(),
                issue_number: None,
                pr_number: Some(42),
                repo: None,
                priority: None,
                metadata: Some(serde_json::json!({ "note": "manual" })),
            })
            .unwrap();
        assert_eq!(rev.queue_item.dedupe_key, "reviewer-proj-1-pr-42");
        assert_eq!(rev.queue_item.pr_number, Some(42));
        assert_eq!(rev.loop_record.r#type, "reviewer");

        let fix = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "fixer".into(),
                issue_number: None,
                pr_number: Some(42),
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap();
        assert_eq!(fix.queue_item.dedupe_key, "fixer-proj-1-pr-42");
        assert_ne!(rev.loop_record.id, fix.loop_record.id, "roles get distinct loops");

        // Re-admit fixer is idempotent
        let fix2 = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "fixer".into(),
                issue_number: None,
                pr_number: Some(42),
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap();
        assert!(!fix2.created_new_loop);
        assert_eq!(fix.queue_item.id, fix2.queue_item.id);
    }

    #[test]
    fn admit_work_worker_issue_and_pr() {
        let repos = setup();
        seed_project(&repos, "proj-1", Some("acme/widget"));
        let s = svc(repos);

        let by_issue = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "worker".into(),
                issue_number: Some(7),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap();
        assert_eq!(by_issue.queue_item.dedupe_key, "worker-proj-1-issue-7");

        let by_pr = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "worker".into(),
                issue_number: None,
                pr_number: Some(9),
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap();
        assert_eq!(by_pr.queue_item.dedupe_key, "worker-proj-1-pr-9");
    }

    #[test]
    fn admit_work_worker_missing_target() {
        let repos = setup();
        seed_project(&repos, "proj-1", Some("acme/widget"));
        let s = svc(repos);

        let err = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "worker".into(),
                issue_number: None,
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap_err();
        assert!(err.to_string().contains("issue_number or pr_number"));
    }

    #[test]
    fn admit_work_reuses_discovery_style_loop() {
        // Conflict / reuse: an existing active loop with bare target_id (as
        // discovery creates) must be reused, not rejected as ActiveLoopConflict.
        let repos = setup();
        seed_project(&repos, "proj-1", Some("acme/widget"));

        let seq = repos.loops.allocate_seq().unwrap();
        let existing = LoopRecord {
            id: "loop-existing".into(),
            seq,
            project_id: "proj-1".into(),
            r#type: "planner".into(),
            target_type: "issue".into(),
            target_id: Some("12".into()),
            repo: Some("acme/widget".into()),
            pr_number: None,
            status: "queued".into(),
            config_json: None,
            metadata_json: None,
            last_run_at: None,
            next_run_at: None,
            created_at: "2024-01-01T00:00:00.000Z".into(),
            updated_at: "2024-01-01T00:00:00.000Z".into(),
        };
        repos.loops.upsert(&existing).unwrap();

        let s = svc(repos);
        let result = s
            .admit_work(AdmitWorkInput {
                project_id: "proj-1".into(),
                role: "planner".into(),
                issue_number: Some(12),
                pr_number: None,
                repo: None,
                priority: None,
                metadata: None,
            })
            .unwrap();

        assert!(!result.created_new_loop);
        assert_eq!(result.loop_record.id, "loop-existing");
        assert_eq!(result.queue_item.loop_id.as_deref(), Some("loop-existing"));
    }
}
