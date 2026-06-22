use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::info;

use looper_storage::{
    eventlog::new_event_id,
    record::LoopRecord,
    repos::Repositories,
};
use looper_types::{
    loop_target_key, LoopStatus, LoopTarget, LoopType,
};

use crate::error::{Result, ServiceError};

/// ── LoopService ──────────────────────────────────────────────────────────
pub struct LoopService {
    repos: Arc<Repositories>,
    now: Box<dyn Fn() -> DateTime<Utc>>,
}

impl LoopService {
    pub fn new<F>(repos: Arc<Repositories>, now: F) -> Self
    where
        F: Fn() -> DateTime<Utc> + 'static,
    {
        Self {
            repos,
            now: Box::new(now),
        }
    }

    // ── Create ──────────────────────────────────────────────────────────

    pub fn create(&self, input: CreateInput) -> Result<LoopRecord> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        // 1. Validate loop type / target type compatibility
        if !input.r#type.supports_target_type(input.target.target_type) {
            return Err(ServiceError::Other(format!(
                "loop type {} cannot target {}",
                input.r#type.as_str(),
                input.target.target_type.as_str(),
            )));
        }

        // 2. Check project exists
        let _project = self
            .repos
            .projects
            .get_by_id(&input.project_id)?
            .ok_or_else(|| ServiceError::ProjectNotFound(input.project_id.clone()))?;

        // 3. Check unique active-loop constraint
        let target_key = loop_target_key(&input.target);
        if is_schedulable_status_str(input.status.as_str()) {
            let all_loops = self.repos.loops.list()?;
            let conflict = all_loops.iter().any(|l| {
                l.project_id == input.project_id
                    && l.r#type == input.r#type.as_str()
                    && l.target_id.as_deref() == Some(&target_key)
                    && is_schedulable_status_str(&l.status)
            });
            if conflict {
                return Err(ServiceError::ActiveLoopConflict {
                    project_id: input.project_id,
                    loop_type: input.r#type.as_str().to_string(),
                    target_key,
                });
            }
        }

        // 4. Allocate seq
        let seq = self.repos.loops.allocate_seq()?;

        // 5. Build LoopRecord
        let next_run_at = if input.status == LoopStatus::Running {
            Some(now_iso.clone())
        } else {
            None
        };

        let record = LoopRecord {
            id: new_event_id("loop"),
            seq,
            project_id: input.project_id,
            r#type: input.r#type.as_str().to_string(),
            target_type: input.target.target_type.as_str().to_string(),
            target_id: Some(target_key),
            repo: input.target.repo,
            pr_number: input.target.number,
            status: input.status.as_str().to_string(),
            config_json: input.config_json,
            metadata_json: input.metadata_json,
            last_run_at: None,
            next_run_at,
            created_at: now_iso.clone(),
            updated_at: now_iso,
        };

        // 6. Upsert
        self.repos.loops.upsert(&record)?;
        info!(loop_id = %record.id, seq = %record.seq, status = %record.status, "loop created");

        Ok(record)
    }

    // ── Get / GetBySeq / List ───────────────────────────────────────────

    pub fn get(&self, id: &str) -> Result<Option<LoopRecord>> {
        Ok(self.repos.loops.get_by_id(id)?)
    }

    pub fn get_by_seq(&self, seq: i64) -> Result<Option<LoopRecord>> {
        Ok(self.repos.loops.get_by_seq(seq)?)
    }

    pub fn list(&self) -> Result<Vec<LoopRecord>> {
        Ok(self.repos.loops.list()?)
    }

    // ── TransitionStatus ────────────────────────────────────────────────

    pub fn transition_status(
        &self,
        id: &str,
        input: TransitionInput,
    ) -> Result<LoopRecord> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        // 1. Fetch loop
        let mut record = self
            .repos
            .loops
            .get_by_id(id)?
            .ok_or_else(|| ServiceError::LoopNotFound(id.to_string()))?;

        // 2. Validate transition via domain state machine
        let from_status: LoopStatus = record.status.parse()?;
        if !from_status.can_transition_to(input.status) {
            return Err(ServiceError::Other(format!(
                "invalid loop status transition: {} → {}",
                from_status.as_str(),
                input.status.as_str(),
            )));
        }

        // 3. Update timestamps & status
        record.status = input.status.as_str().to_string();
        record.updated_at = now_iso.clone();

        // Compute next_run_at
        record.next_run_at = if let Some(explicit) = input.next_run_at {
            Some(explicit.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        } else if input.status == LoopStatus::Queued {
            Some(now_iso.clone())
        } else if input.status == LoopStatus::Running {
            record.next_run_at.take()
        } else {
            None
        };

        // last_run_at
        if let Some(lra) = input.last_run_at {
            record.last_run_at = Some(lra.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());
        }

        // 4. Upsert
        self.repos.loops.upsert(&record)?;
        info!(loop_id = %id, new_status = %record.status, "loop status transitioned");

        Ok(record)
    }

    // ── Pause ───────────────────────────────────────────────────────────

    pub fn pause(&self, input: PauseInput) -> Result<PauseResult> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let mut record = self
            .repos
            .loops
            .get_by_id(&input.loop_id)?
            .ok_or_else(|| ServiceError::LoopNotFound(input.loop_id.clone()))?;

        let from_status: LoopStatus = record.status.parse()?;

        // If not already paused, validate transition
        if from_status != LoopStatus::Paused
            && !from_status.can_transition_to(LoopStatus::Paused) {
                return Err(ServiceError::Other(format!(
                    "cannot pause loop in status {}",
                    from_status.as_str(),
                )));
            }

        record.status = LoopStatus::Paused.as_str().to_string();
        record.next_run_at = None;
        record.updated_at = now_iso;

        self.repos.loops.upsert(&record)?;

        // Cancel active queue items for this loop
        let cancelled_queue_items = self
            .repos
            .queue
            .cancel_by_loop(&input.loop_id, &now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(), input.reason.as_deref())?;

        info!(
            loop_id = %input.loop_id,
            cancelled = cancelled_queue_items,
            "loop paused"
        );

        Ok(PauseResult {
            r#loop: record,
            cancelled_queue_items,
        })
    }

    // ── Terminate ───────────────────────────────────────────────────────

    pub fn terminate(&self, input: TerminateInput) -> Result<TerminateResult> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let mut record = self
            .repos
            .loops
            .get_by_id(&input.loop_id)?
            .ok_or_else(|| ServiceError::LoopNotFound(input.loop_id.clone()))?;

        let from_status: LoopStatus = record.status.parse()?;

        if !from_status.can_transition_to(LoopStatus::Terminated) {
            return Err(ServiceError::Other(format!(
                "cannot terminate loop in status {}",
                from_status.as_str(),
            )));
        }

        record.status = LoopStatus::Terminated.as_str().to_string();
        record.next_run_at = None;
        record.updated_at = now_iso;

        self.repos.loops.upsert(&record)?;

        let cancelled_queue_items = self
            .repos
            .queue
            .cancel_by_loop(&input.loop_id, &now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(), input.reason.as_deref())?;

        info!(
            loop_id = %input.loop_id,
            cancelled = cancelled_queue_items,
            "loop terminated"
        );

        Ok(TerminateResult {
            r#loop: record,
            cancelled_queue_items,
        })
    }

    // ── Resume ──────────────────────────────────────────────────────────

    pub fn resume(&self, loop_id: &str) -> Result<LoopRecord> {
        self.transition_status(
            loop_id,
            TransitionInput {
                status: LoopStatus::Queued,
                next_run_at: Some((self.now)()),
                last_run_at: None,
            },
        )
    }

    // ── Policy helpers ──────────────────────────────────────────────────

    pub fn normalize_resume_policy(
        failure_kind: &str,
        resume_policy: Option<&str>,
    ) -> &'static str {
        match (failure_kind, resume_policy) {
            (_, Some(policy)) if !policy.trim().is_empty() => {
                // Return the policy string — it will leak in static context.
                // Callers should use the well-known constants.
                // At runtime, we return a &str; for now we match known values.
                match policy.trim() {
                    "advance_from_checkpoint" => "advance_from_checkpoint",
                    "manual_intervention" => "manual_intervention",
                    "replay_step" => "replay_step",
                    "restart_from_discover" => "restart_from_discover",
                    "rerun_review" => "rerun_review",
                    "retry_from_timeout_context" => "retry_from_timeout_context",
                    _ => "replay_step",
                }
            }
            ("retryable_after_resume", _) => "advance_from_checkpoint",
            ("manual_intervention", _) => "manual_intervention",
            _ => "replay_step",
        }
    }

    pub fn suppresses_autonomous_recovery(
        failure_kind: &str,
        resume_policy: &str,
    ) -> bool {
        resume_policy.trim() == "manual_intervention"
            || failure_kind.trim() == "manual_intervention"
    }

    pub fn should_restart_from_discover(status: &str, resume_policy: &str) -> bool {
        if status != "failed" && status != "interrupted" {
            return false;
        }
        resume_policy.trim() == "restart_from_discover"
    }
}

/// ── Input / Output types ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CreateInput {
    pub project_id: String,
    pub r#type: LoopType,
    pub target: LoopTarget,
    pub status: LoopStatus,
    pub config_json: Option<String>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TransitionInput {
    pub status: LoopStatus,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct PauseInput {
    pub loop_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PauseResult {
    pub r#loop: LoopRecord,
    pub cancelled_queue_items: i64,
}

#[derive(Debug, Clone)]
pub struct TerminateInput {
    pub loop_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TerminateResult {
    pub r#loop: LoopRecord,
    pub cancelled_queue_items: i64,
}

// ── Internal helpers ────────────────────────────────────────────────────

/// Statuses that participate in the active-loop scheduling set.
fn is_schedulable_status_str(s: &str) -> bool {
    matches!(s, "idle" | "queued" | "running" | "paused")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::TimeZone;
    use looper_storage::migration::run_migrations;
    use looper_storage::record::ProjectRecord;
    use looper_storage::repos::Repositories;
    use looper_types::loop_target::LoopTargetType;
    use rusqlite::Connection;

    use super::*;

    /// Create in-memory SQLite repos with WAL + FK + migrations.
    fn setup() -> Arc<Repositories> {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        run_migrations(&mut conn).unwrap();
        Arc::new(Repositories::new(conn))
    }

    fn seed_project(repos: &Repositories) -> ProjectRecord {
        let p = ProjectRecord {
            id: "proj-1".into(),
            name: "test".into(),
            repo_path: "/tmp/p".into(),
            metadata_json: None,
            base_branch: Some("main".into()),
            archived: false,
            created_at: "2024-01-01T00:00:00.000Z".into(),
            updated_at: "2024-01-01T00:00:00.000Z".into(),
        };
        repos.projects.upsert(&p).unwrap();
        p
    }

    fn svc(repos: Arc<Repositories>) -> LoopService {
        LoopService::new(repos, Utc::now)
    }

    fn base_create(project_id: &str) -> CreateInput {
        CreateInput {
            project_id: project_id.into(),
            r#type: LoopType::Planner,
            target: LoopTarget {
                target_type: LoopTargetType::Project,
                project_id: Some(project_id.into()),
                repo: None,
                number: None,
            },
            status: LoopStatus::Idle,
            config_json: None,
            metadata_json: None,
        }
    }

    #[test]
    fn test_is_schedulable_status() {
        assert!(is_schedulable_status_str("idle"));
        assert!(is_schedulable_status_str("queued"));
        assert!(is_schedulable_status_str("running"));
        assert!(is_schedulable_status_str("paused"));
        assert!(!is_schedulable_status_str("completed"));
        assert!(!is_schedulable_status_str("failed"));
        assert!(!is_schedulable_status_str("cancelled"));
        assert!(!is_schedulable_status_str("terminated"));
    }

    #[test]
    fn test_create_input_basic() {
        let input = base_create("proj-1");
        assert_eq!(input.project_id, "proj-1");
        assert_eq!(input.r#type, LoopType::Planner);
    }

    #[test]
    fn test_create_loop_success() {
        let repos = setup();
        seed_project(&repos);
        let s = svc(repos.clone());
        let loop_ = s.create(base_create("proj-1")).unwrap();
        assert_eq!(loop_.project_id, "proj-1");
        assert_eq!(loop_.status, "idle");
        assert!(loop_.seq > 0);
    }

    #[test]
    fn test_create_loop_rejects_missing_project() {
        let repos = setup();
        let s = svc(repos);
        let err = s.create(base_create("nonexistent")).unwrap_err();
        assert!(matches!(err, ServiceError::ProjectNotFound(_)));
    }

    #[test]
    fn test_create_loop_enforces_active_conflict() {
        let repos = setup();
        seed_project(&repos);
        let s = svc(repos.clone());
        s.create(base_create("proj-1")).unwrap();
        // Second create with same project+type+target should conflict
        let err = s.create(base_create("proj-1")).unwrap_err();
        assert!(matches!(err, ServiceError::ActiveLoopConflict { .. }));
    }

    #[test]
    fn test_get_loop_returns_none_for_missing() {
        let repos = setup();
        let s = svc(repos);
        assert!(s.get("nope").unwrap().is_none());
    }

    #[test]
    fn test_create_then_get_by_id() {
        let repos = setup();
        seed_project(&repos);
        let s = svc(repos.clone());
        let created = s.create(base_create("proj-1")).unwrap();
        let fetched = s.get(&created.id).unwrap().unwrap();
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.status, "idle");
    }

    #[test]
    fn test_create_then_get_by_seq() {
        let repos = setup();
        seed_project(&repos);
        let s = svc(repos.clone());
        let created = s.create(base_create("proj-1")).unwrap();
        let fetched = s.get_by_seq(created.seq).unwrap().unwrap();
        assert_eq!(fetched.id, created.id);
    }

    #[test]
    fn test_create_then_list() {
        let repos = setup();
        seed_project(&repos);
        let s = svc(repos.clone());
        s.create(base_create("proj-1")).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn test_transition_status_valid() {
        let repos = setup();
        seed_project(&repos);
        let s = svc(repos.clone());
        let loop_ = s.create(base_create("proj-1")).unwrap();
        let updated = s
            .transition_status(
                &loop_.id,
                TransitionInput {
                    status: LoopStatus::Queued,
                    next_run_at: None,
                    last_run_at: None,
                },
            )
            .unwrap();
        assert_eq!(updated.status, "queued");
    }

    #[test]
    fn test_transition_status_invalid_rejected() {
        let repos = setup();
        seed_project(&repos);
        let s = svc(repos.clone());
        let loop_ = s.create(base_create("proj-1")).unwrap();
        let err = s
            .transition_status(
                &loop_.id,
                TransitionInput {
                    status: LoopStatus::Running,
                    next_run_at: None,
                    last_run_at: None,
                },
            )
            .unwrap_err();
        // idle → running is not valid
        assert!(matches!(err, ServiceError::Other(_)));
    }

    #[test]
    fn test_pause_loop() {
        let repos = setup();
        seed_project(&repos);
        let s = svc(repos.clone());
        let mut loop_ = s.create(base_create("proj-1")).unwrap();
        // idle → queued → running before pause
        loop_ = s.transition_status(&loop_.id, TransitionInput {
            status: LoopStatus::Queued, next_run_at: None, last_run_at: None,
        }).unwrap();
        loop_ = s.transition_status(&loop_.id, TransitionInput {
            status: LoopStatus::Running, next_run_at: None, last_run_at: None,
        }).unwrap();
        let result = s
            .pause(PauseInput {
                loop_id: loop_.id.clone(),
                reason: Some("testing".into()),
            })
            .unwrap();
        assert_eq!(result.r#loop.status, "paused");
    }

    #[test]
    fn test_pause_nonexistent_loop() {
        let repos = setup();
        let s = svc(repos);
        let err = s
            .pause(PauseInput {
                loop_id: "nope".into(),
                reason: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::LoopNotFound(_)));
    }

    #[test]
    fn test_terminate_loop() {
        let repos = setup();
        seed_project(&repos);
        let s = svc(repos.clone());
        // Must go idle → queued → running before terminated is reachable
        let mut loop_ = s.create(base_create("proj-1")).unwrap();
        // transition to queued
        loop_ = s.transition_status(&loop_.id, TransitionInput {
            status: LoopStatus::Queued, next_run_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            last_run_at: None,
        }).unwrap();
        // transition to running
        loop_ = s.transition_status(&loop_.id, TransitionInput {
            status: LoopStatus::Running, next_run_at: None, last_run_at: None,
        }).unwrap();
        // now terminate from running
        let result = s
            .terminate(TerminateInput {
                loop_id: loop_.id.clone(),
                reason: Some("done".into()),
            })
            .unwrap();
        assert_eq!(result.r#loop.status, "terminated");
    }

    #[test]
    fn test_resume_loop() {
        let repos = setup();
        seed_project(&repos);
        let s = svc(repos.clone());
        let mut loop_ = s.create(base_create("proj-1")).unwrap();
        // idle → queued → running → paused
        loop_ = s.transition_status(&loop_.id, TransitionInput {
            status: LoopStatus::Queued, next_run_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            last_run_at: None,
        }).unwrap();
        loop_ = s.transition_status(&loop_.id, TransitionInput {
            status: LoopStatus::Running, next_run_at: None, last_run_at: None,
        }).unwrap();
        let paused = s
            .pause(PauseInput {
                loop_id: loop_.id.clone(),
                reason: None,
            })
            .unwrap();
        assert_eq!(paused.r#loop.status, "paused");
        // resume → queued
        let resumed = s.resume(&loop_.id).unwrap();
        assert_eq!(resumed.status, "queued");
    }

    #[test]
    fn test_create_loop_multiple_projects_no_conflict() {
        let repos = setup();
        let p1 = ProjectRecord {
            id: "proj-a".into(),
            name: "a".into(),
            repo_path: "/tmp/a".into(),
            metadata_json: None,
            base_branch: Some("main".into()),
            archived: false,
            created_at: "2024-01-01T00:00:00.000Z".into(),
            updated_at: "2024-01-01T00:00:00.000Z".into(),
        };
        let p2 = ProjectRecord {
            id: "proj-b".into(),
            name: "b".into(),
            repo_path: "/tmp/b".into(),
            metadata_json: None,
            base_branch: Some("main".into()),
            archived: false,
            created_at: "2024-01-01T00:00:00.000Z".into(),
            updated_at: "2024-01-01T00:00:00.000Z".into(),
        };
        repos.projects.upsert(&p1).unwrap();
        repos.projects.upsert(&p2).unwrap();
        let s = svc(repos.clone());

        // Same loop type + target for different projects → no conflict
        s.create(base_create("proj-a")).unwrap();
        s.create(base_create("proj-b")).unwrap();
        assert_eq!(s.list().unwrap().len(), 2);
    }

    #[test]
    fn test_normalize_resume_policy_defaults() {
        assert_eq!(
            LoopService::normalize_resume_policy("retryable_after_resume", None),
            "advance_from_checkpoint"
        );
        assert_eq!(
            LoopService::normalize_resume_policy("unknown", None),
            "replay_step"
        );
        assert_eq!(
            LoopService::normalize_resume_policy("x", Some("manual_intervention")),
            "manual_intervention"
        );
    }

    #[test]
    fn test_suppresses_autonomous_recovery() {
        assert!(LoopService::suppresses_autonomous_recovery("x", "manual_intervention"));
        assert!(LoopService::suppresses_autonomous_recovery("manual_intervention", "x"));
        assert!(!LoopService::suppresses_autonomous_recovery("x", "replay_step"));
    }

    #[test]
    fn test_should_restart_from_discover() {
        assert!(LoopService::should_restart_from_discover("failed", "restart_from_discover"));
        assert!(!LoopService::should_restart_from_discover("idle", "restart_from_discover"));
        assert!(!LoopService::should_restart_from_discover("failed", "replay_step"));
    }
}
