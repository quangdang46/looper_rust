use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::json;
use tracing::info;

use looper_storage::{
    eventlog::{append, new_event_id},
    record::{AppendInput, RunRecord},
    repos::Repositories,
};
use looper_types::{assert_step_belongs_to_loop_type, LoopStatus, LoopType, RunStatus};

use crate::error::{Result, ServiceError};
use crate::loop_service::LoopService;

/// ── RunService ──────────────────────────────────────────────────────────
pub struct RunService {
    repos: Arc<Repositories>,
    _loops: Arc<LoopService>,
    now: Box<dyn Fn() -> DateTime<Utc>>,
}

impl RunService {
    pub fn new<F>(repos: Arc<Repositories>, loops: Arc<LoopService>, now: F) -> Self
    where
        F: Fn() -> DateTime<Utc> + 'static,
    {
        Self { repos, _loops: loops, now: Box::new(now) }
    }

    // ── StartRun ────────────────────────────────────────────────────────

    pub fn start_run(&self, input: StartInput) -> Result<RunRecord> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        // 1. Fetch loop
        let mut loop_record = self
            .repos
            .loops
            .get_by_id(&input.loop_id)?
            .ok_or_else(|| ServiceError::LoopNotFound(input.loop_id.clone()))?;

        // 2. OneRunningRunPerLoop enforcement
        if self.repos.runs.has_running_by_loop_id(&input.loop_id)? {
            return Err(ServiceError::LoopHasRunningRun { loop_id: input.loop_id.clone() });
        }

        // 3. Validate loop status transition
        let loop_status: LoopStatus = loop_record.status.parse()?;
        if loop_status != LoopStatus::Running && !loop_status.can_transition_to(LoopStatus::Running) {
            return Err(ServiceError::Other(format!(
                "cannot start run: loop status {} cannot transition to running",
                loop_status.as_str(),
            )));
        }

        // 4. Validate steps if provided
        let loop_type: LoopType = loop_record.r#type.parse()?;
        if let Some(ref step) = input.current_step {
            assert_step_belongs_to_loop_type(loop_type, step).map_err(|e| ServiceError::Other(e.to_string()))?;
        }
        if let Some(ref step) = input.last_completed_step {
            assert_step_belongs_to_loop_type(loop_type, step).map_err(|e| ServiceError::Other(e.to_string()))?;
        }

        // 5. Update loop status BEFORE creating the run (otherwise
        //    INSERT OR REPLACE on loops triggers ON DELETE CASCADE on runs)
        loop_record.status = LoopStatus::Running.as_str().to_string();
        loop_record.last_run_at = Some(now_iso.clone());
        loop_record.next_run_at = None;
        loop_record.updated_at = now_iso.clone();
        self.repos.loops.upsert(&loop_record)?;

        // 6. Create RunRecord
        let run_record = RunRecord {
            id: new_event_id("run"),
            loop_id: input.loop_id.clone(),
            status: RunStatus::Running.as_str().to_string(),
            current_step: input.current_step,
            last_completed_step: input.last_completed_step,
            checkpoint_json: input.checkpoint_json,
            summary: None,
            error_message: None,
            agent_vendor: None,
            model: None,
            started_at: now_iso.clone(),
            last_heartbeat_at: Some(now_iso.clone()),
            ended_at: None,
            created_at: now_iso.clone(),
            updated_at: now_iso.clone(),
        };

        // 7. Upsert run record
        self.repos.runs.upsert(&run_record)?;

        // 8. Side effect — Event log (non-fatal: observability shouldn't block the run)
        if let Err(e) = append(
            &self.repos.events,
            &AppendInput {
                event_type: "loop.started".into(),
                project_id: Some(loop_record.project_id.clone()),
                loop_id: Some(input.loop_id.clone()),
                run_id: Some(run_record.id.clone()),
                entity_type: Some("loop".into()),
                entity_id: Some(loop_record.id.clone()),
                payload_json: Some(json!({ "status": "running" }).to_string()),
                ..AppendInput::new("")
            },
        ) {
            tracing::warn!(error = %e, "Failed to emit loop.started event");
        }

        if let Err(e) = append(
            &self.repos.events,
            &AppendInput {
                event_type: "run.started".into(),
                project_id: Some(loop_record.project_id.clone()),
                loop_id: Some(input.loop_id.clone()),
                run_id: Some(run_record.id.clone()),
                entity_type: Some("run".into()),
                entity_id: Some(run_record.id.clone()),
                payload_json: Some(
                    json!({
                        "currentStep": run_record.current_step,
                        "lastCompletedStep": run_record.last_completed_step,
                    })
                    .to_string(),
                ),
                ..AppendInput::new("")
            },
        ) {
            tracing::warn!(error = %e, "Failed to emit run.started event");
        }

        info!(
            run_id = %run_record.id,
            loop_id = %input.loop_id,
            "run started"
        );

        Ok(run_record)
    }

    // ── RecordStep ──────────────────────────────────────────────────────

    pub fn record_step(&self, input: RecordStepInput) -> Result<RunRecord> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        // 1. Fetch run
        let mut run_record =
            self.repos.runs.get_by_id(&input.run_id)?.ok_or_else(|| ServiceError::RunNotFound(input.run_id.clone()))?;

        // 2. Validate steps if provided
        if let Some(ref step) = input.current_step {
            assert_step_belongs_to_loop_type(input.loop_type, step).map_err(|e| ServiceError::Other(e.to_string()))?;
        }
        if let Some(ref step) = input.last_completed_step {
            assert_step_belongs_to_loop_type(input.loop_type, step).map_err(|e| ServiceError::Other(e.to_string()))?;
        }

        // 3. Update run record
        if let Some(cs) = input.current_step {
            run_record.current_step = Some(cs);
        }
        if let Some(lcs) = input.last_completed_step {
            run_record.last_completed_step = Some(lcs);
        }
        if let Some(cj) = input.checkpoint_json {
            run_record.checkpoint_json = Some(cj);
        }
        run_record.last_heartbeat_at = Some(
            input
                .last_heartbeat_at
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
                .unwrap_or(now_iso.clone()),
        );
        run_record.updated_at = now_iso.clone();

        // 4. Fetch loop for project_id
        let loop_record = self.repos.loops.get_by_id(&run_record.loop_id)?;

        // 5. Side effect — Event log (non-fatal)
        if let Some(event_type) = input.event_type {
            let payload = input.event_payload.unwrap_or_else(|| json!({})).to_string();

            if let Err(e) = append(
                &self.repos.events,
                &AppendInput {
                    event_type,
                    project_id: loop_record.as_ref().map(|l| l.project_id.clone()),
                    loop_id: Some(run_record.loop_id.clone()),
                    run_id: Some(input.run_id.clone()),
                    entity_type: Some("run".into()),
                    entity_id: Some(run_record.id.clone()),
                    payload_json: Some(payload),
                    ..AppendInput::new("")
                },
            ) {
                tracing::warn!(error = %e, "Failed to emit record_step event");
            }
        }

        // 6. Upsert
        self.repos.runs.upsert(&run_record)?;

        Ok(run_record)
    }

    // ── Complete ────────────────────────────────────────────────────────

    pub fn complete(&self, run_id: &str, input: CompleteInput) -> Result<RunRecord> {
        let now = (self.now)();
        let now_iso = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        // 1. Fetch run
        let mut run_record =
            self.repos.runs.get_by_id(run_id)?.ok_or_else(|| ServiceError::RunNotFound(run_id.to_string()))?;

        // 2. Validate run status transition
        let from_status: RunStatus = run_record.status.parse()?;
        if !from_status.can_transition_to(input.status) {
            return Err(ServiceError::Other(format!(
                "invalid run status transition: {} → {}",
                from_status.as_str(),
                input.status.as_str(),
            )));
        }

        // 3. Update run record
        run_record.status = input.status.as_str().to_string();
        run_record.summary = input.summary;
        run_record.error_message = input.error_message;
        run_record.checkpoint_json = input.checkpoint_json;
        run_record.ended_at = Some(now_iso.clone());
        run_record.last_heartbeat_at = Some(now_iso.clone());
        run_record.updated_at = now_iso.clone();

        // 4. Fetch loop for project_id
        let loop_record = self.repos.loops.get_by_id(&run_record.loop_id)?;

        // 5. Side effect — Event log (non-fatal)
        let event_type = if input.status == RunStatus::Success { "run.completed" } else { "run.failed" };

        let payload = json!({
            "summary": run_record.summary,
            "errorMessage": run_record.error_message,
        });

        if let Err(e) = append(
            &self.repos.events,
            &AppendInput {
                event_type: event_type.into(),
                project_id: loop_record.as_ref().map(|l| l.project_id.clone()),
                loop_id: Some(run_record.loop_id.clone()),
                run_id: Some(run_id.to_string()),
                entity_type: Some("run".into()),
                entity_id: Some(run_record.id.clone()),
                payload_json: Some(payload.to_string()),
                ..AppendInput::new("")
            },
        ) {
            tracing::warn!(error = %e, "Failed to emit run complete event");
        }

        // 6. Upsert
        self.repos.runs.upsert(&run_record)?;

        info!(
            run_id = %run_id,
            status = %run_record.status,
            "run completed"
        );

        Ok(run_record)
    }

    // ── Query methods ───────────────────────────────────────────────────

    pub fn get(&self, id: &str) -> Result<Option<RunRecord>> {
        Ok(self.repos.runs.get_by_id(id)?)
    }

    pub fn list(&self) -> Result<Vec<RunRecord>> {
        Ok(self.repos.runs.list()?)
    }

    pub fn list_by_loop(&self, loop_id: &str) -> Result<Vec<RunRecord>> {
        Ok(self.repos.runs.list_by_loop(loop_id)?)
    }

    pub fn latest_for_loop(&self, loop_id: &str) -> Result<Option<RunRecord>> {
        Ok(self.repos.runs.get_latest_by_loop_id(loop_id)?)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use rusqlite::Connection;

    use looper_storage::migration::run_migrations;
    use looper_storage::record::{LoopRecord, ProjectRecord};
    use looper_storage::repos::Repositories;

    use looper_types::{LoopType, RunStatus};

    use super::*;
    use crate::loop_service::LoopService;

    fn repos_setup() -> Arc<Repositories> {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;").unwrap();
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

    fn seed_loop(repos: &Repositories) -> LoopRecord {
        let l = LoopRecord {
            id: "loop-1".into(),
            seq: 1,
            project_id: "proj-1".into(),
            r#type: "planner".into(),
            target_type: "project".into(),
            target_id: Some("proj-1".into()),
            repo: None,
            pr_number: None,
            status: "queued".into(),
            config_json: None,
            metadata_json: None,
            last_run_at: None,
            next_run_at: None,
            created_at: "2024-01-01T00:00:00.000Z".into(),
            updated_at: "2024-01-01T00:00:00.000Z".into(),
        };
        repos.loops.upsert(&l).unwrap();
        l
    }

    #[test]
    fn test_start_input_defaults() {
        let input = StartInput {
            loop_id: "loop-1".into(),
            current_step: Some("discover".into()),
            last_completed_step: None,
            checkpoint_json: None,
        };
        assert_eq!(input.loop_id, "loop-1");
        assert_eq!(input.current_step.as_deref(), Some("discover"));
    }

    #[test]
    fn test_record_step_input_roundtrip() {
        let input = RecordStepInput {
            run_id: "run-1".into(),
            loop_type: LoopType::Planner,
            current_step: Some("prepare".into()),
            last_completed_step: Some("discover".into()),
            checkpoint_json: Some(r#"{"phase":1}"#.into()),
            last_heartbeat_at: None,
            event_type: Some("step_completed".into()),
            event_payload: None,
        };
        assert_eq!(input.run_id, "run-1");
        assert_eq!(input.loop_type, LoopType::Planner);
    }

    #[test]
    fn test_complete_input_variants() {
        let ok_input = CompleteInput {
            status: RunStatus::Success,
            summary: Some("All good".into()),
            error_message: None,
            checkpoint_json: None,
        };
        assert_eq!(ok_input.status, RunStatus::Success);

        let fail_input = CompleteInput {
            status: RunStatus::Failed,
            summary: Some("Step 2 failed".into()),
            error_message: Some("Timeout".into()),
            checkpoint_json: None,
        };
        assert_eq!(fail_input.status, RunStatus::Failed);
        assert_eq!(fail_input.error_message.as_deref(), Some("Timeout"));
    }

    #[test]
    fn test_run_status_values() {
        assert_eq!(RunStatus::Running.as_str(), "running");
        assert_eq!(RunStatus::Success.as_str(), "success");
        assert_eq!(RunStatus::Failed.as_str(), "failed");
        assert!(RunStatus::Success.is_terminal());
        assert!(!RunStatus::Running.is_terminal());
    }

    // ── Business-logic tests ─────────────────────────────────────────

    #[test]
    fn test_start_run_missing_loop() {
        let repos = repos_setup();
        let loop_svc = Arc::new(LoopService::new(repos.clone(), Utc::now));
        let run_svc = RunService::new(repos, loop_svc, Utc::now);
        let err = run_svc
            .start_run(StartInput {
                loop_id: "nope".into(),
                current_step: None,
                last_completed_step: None,
                checkpoint_json: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::LoopNotFound(_)));
    }

    #[test]
    fn test_start_run_success() {
        let repos = repos_setup();
        seed_project(&repos);
        let loop_ = seed_loop(&repos);
        let loop_svc = Arc::new(LoopService::new(repos.clone(), Utc::now));
        let run_svc = RunService::new(repos, loop_svc, Utc::now);
        let run = run_svc
            .start_run(StartInput {
                loop_id: loop_.id.clone(),
                current_step: Some("discover".into()),
                last_completed_step: None,
                checkpoint_json: None,
            })
            .unwrap();
        assert_eq!(run.loop_id, loop_.id);
        assert_eq!(run.status, "running");
    }

    #[test]
    fn test_start_run_rejects_second_run() {
        let repos = repos_setup();
        seed_project(&repos);
        let loop_ = seed_loop(&repos);
        let loop_svc = Arc::new(LoopService::new(repos.clone(), Utc::now));
        let run_svc = RunService::new(repos.clone(), loop_svc, Utc::now);
        run_svc
            .start_run(StartInput {
                loop_id: loop_.id.clone(),
                current_step: None,
                last_completed_step: None,
                checkpoint_json: None,
            })
            .unwrap();
        let err = run_svc
            .start_run(StartInput {
                loop_id: loop_.id.clone(),
                current_step: None,
                last_completed_step: None,
                checkpoint_json: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::LoopHasRunningRun { .. }));
    }

    #[test]
    fn test_start_run_invalid_step_rejected() {
        let repos = repos_setup();
        seed_project(&repos);
        let loop_ = seed_loop(&repos);
        let loop_svc = Arc::new(LoopService::new(repos.clone(), Utc::now));
        let run_svc = RunService::new(repos, loop_svc, Utc::now);
        let err = run_svc
            .start_run(StartInput {
                loop_id: loop_.id.clone(),
                current_step: Some("nonexistent-step".into()),
                last_completed_step: None,
                checkpoint_json: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::Other(_)));
    }

    #[test]
    fn test_record_step_updates_run() {
        let repos = repos_setup();
        seed_project(&repos);
        let loop_ = seed_loop(&repos);
        let loop_svc = Arc::new(LoopService::new(repos.clone(), Utc::now));
        let run_svc = RunService::new(repos.clone(), loop_svc, Utc::now);
        let run = run_svc
            .start_run(StartInput {
                loop_id: loop_.id.clone(),
                current_step: Some("discover".into()),
                last_completed_step: None,
                checkpoint_json: None,
            })
            .unwrap();
        let updated = run_svc
            .record_step(RecordStepInput {
                run_id: run.id.clone(),
                loop_type: LoopType::Planner,
                current_step: Some("assess".into()),
                last_completed_step: Some("discover".into()),
                checkpoint_json: Some(r#"{"phase":2}"#.into()),
                last_heartbeat_at: None,
                event_type: Some("step_completed".into()),
                event_payload: None,
            })
            .unwrap();
        assert_eq!(updated.current_step.as_deref(), Some("assess"));
        assert_eq!(updated.last_completed_step.as_deref(), Some("discover"));
    }

    #[test]
    fn test_complete_run_success() {
        let repos = repos_setup();
        seed_project(&repos);
        let loop_ = seed_loop(&repos);
        let loop_svc = Arc::new(LoopService::new(repos.clone(), Utc::now));
        let run_svc = RunService::new(repos.clone(), loop_svc, Utc::now);
        let run = run_svc
            .start_run(StartInput {
                loop_id: loop_.id.clone(),
                current_step: None,
                last_completed_step: None,
                checkpoint_json: None,
            })
            .unwrap();
        let completed = run_svc
            .complete(
                &run.id,
                CompleteInput {
                    status: RunStatus::Success,
                    summary: Some("All done".into()),
                    error_message: None,
                    checkpoint_json: None,
                },
            )
            .unwrap();
        assert_eq!(completed.status, "success");
        assert_eq!(completed.summary.as_deref(), Some("All done"));
    }

    #[test]
    fn test_list_runs() {
        let repos = repos_setup();
        seed_project(&repos);
        let loop_ = seed_loop(&repos);
        let loop_svc = Arc::new(LoopService::new(repos.clone(), Utc::now));
        let run_svc = RunService::new(repos.clone(), loop_svc, Utc::now);
        run_svc
            .start_run(StartInput {
                loop_id: loop_.id.clone(),
                current_step: None,
                last_completed_step: None,
                checkpoint_json: None,
            })
            .unwrap();
        assert_eq!(run_svc.list().unwrap().len(), 1);
        assert_eq!(run_svc.list_by_loop(&loop_.id).unwrap().len(), 1);
    }

    #[test]
    fn test_latest_for_loop() {
        let repos = repos_setup();
        seed_project(&repos);
        let loop_ = seed_loop(&repos);
        let loop_svc = Arc::new(LoopService::new(repos.clone(), Utc::now));
        let run_svc = RunService::new(repos.clone(), loop_svc, Utc::now);
        let run = run_svc
            .start_run(StartInput {
                loop_id: loop_.id.clone(),
                current_step: None,
                last_completed_step: None,
                checkpoint_json: None,
            })
            .unwrap();
        let latest = run_svc.latest_for_loop(&loop_.id).unwrap().unwrap();
        assert_eq!(latest.id, run.id);
    }
}

/// ── Input types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StartInput {
    pub loop_id: String,
    pub current_step: Option<String>,
    pub last_completed_step: Option<String>,
    pub checkpoint_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecordStepInput {
    pub run_id: String,
    pub loop_type: LoopType,
    pub current_step: Option<String>,
    pub last_completed_step: Option<String>,
    pub checkpoint_json: Option<String>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub event_type: Option<String>,
    pub event_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct CompleteInput {
    pub status: RunStatus,
    pub summary: Option<String>,
    pub error_message: Option<String>,
    pub checkpoint_json: Option<String>,
}
