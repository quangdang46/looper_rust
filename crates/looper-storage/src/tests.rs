use tempfile::TempDir;

use crate::helpers::*;
use crate::migration::run_migrations;
use crate::record::*;
use crate::repos::Repositories;

/// Create an in-memory SQLite database with all migrations applied.
fn setup() -> (Repositories, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("test.db");
    let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
    run_migrations(&mut conn).expect("migrations");
    (Repositories::new(conn), dir)
}

/// Create a test project and loop, returning (project_id, loop_id).
fn insert_project_and_loop(repos: &Repositories) -> (String, String) {
    let t = now();
    let pid = "p-1".to_string();
    let lid = "l-1".to_string();
    repos
        .projects
        .upsert(&ProjectRecord {
            id: pid.clone(),
            name: "test".into(),
            repo_path: "/tmp/r".into(),
            base_branch: None,
            archived: false,
            metadata_json: None,
            created_at: t.clone(),
            updated_at: t.clone(),
        })
        .unwrap();
    repos
        .loops
        .upsert(&LoopRecord {
            id: lid.clone(),
            seq: 1,
            project_id: pid.clone(),
            r#type: "worker".into(),
            target_type: "issue".into(),
            target_id: None,
            repo: None,
            pr_number: None,
            status: "idle".into(),
            config_json: None,
            metadata_json: None,
            last_run_at: None,
            next_run_at: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    (pid, lid)
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string()
}

fn hours_ago(n: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::hours(n)).format("%Y-%m-%dT%H:%M:%S.000Z").to_string()
}

fn make_queue_item(id: &str, r#type: &str, dedupe_key: &str) -> QueueItemRecord {
    let t = now();
    QueueItemRecord {
        id: id.into(),
        project_id: None,
        loop_id: None,
        r#type: r#type.into(),
        target_type: "loop".into(),
        target_id: "l-1".into(),
        repo: Some("org/repo".into()),
        pr_number: None,
        dedupe_key: dedupe_key.into(),
        priority: 1,
        status: "queued".into(),
        available_at: t.clone(),
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
        created_at: t.clone(),
        updated_at: t,
    }
}

#[test]
fn test_projects_upsert_and_get() {
    let (repos, _dir) = setup();
    repos
        .projects
        .upsert(&ProjectRecord {
            id: "p-1".into(),
            name: "test-project".into(),
            repo_path: "/tmp/repo".into(),
            base_branch: Some("main".into()),
            archived: false,
            metadata_json: None,
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    let got = repos.projects.get_by_id("p-1").unwrap().unwrap();
    assert_eq!(got.name, "test-project");
    assert_eq!(got.base_branch.as_deref(), Some("main"));
    assert!(!got.archived);
}

#[test]
fn test_projects_get_missing() {
    let (repos, _dir) = setup();
    assert!(repos.projects.get_by_id("nonexistent").unwrap().is_none());
}

#[test]
fn test_projects_list() {
    let (repos, _dir) = setup();
    let t = now();
    for i in 0..3 {
        repos
            .projects
            .upsert(&ProjectRecord {
                id: format!("p-{i}"),
                name: format!("proj-{i}"),
                repo_path: "/tmp/x".into(),
                base_branch: None,
                archived: false,
                metadata_json: None,
                created_at: t.clone(),
                updated_at: t.clone(),
            })
            .unwrap();
    }
    assert_eq!(repos.projects.list().unwrap().len(), 3);
}

#[test]
fn test_projects_archive() {
    let (repos, _dir) = setup();
    let t = now();
    repos
        .projects
        .upsert(&ProjectRecord {
            id: "p-1".into(),
            name: "x".into(),
            repo_path: "/tmp/x".into(),
            base_branch: None,
            archived: false,
            metadata_json: None,
            created_at: t.clone(),
            updated_at: t.clone(),
        })
        .unwrap();
    assert!(repos.projects.archive("p-1", &now()).unwrap());
    assert!(repos.projects.get_by_id("p-1").unwrap().unwrap().archived);
}

#[test]
fn test_projects_archive_missing() {
    let (repos, _dir) = setup();
    assert!(!repos.projects.archive("no-such", &now()).unwrap());
}

#[test]
fn test_loops_upsert_and_get() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    let t = now();
    repos
        .loops
        .upsert(&LoopRecord {
            id: "loop-42".into(),
            seq: 10,
            project_id: pid,
            r#type: "worker".into(),
            target_type: "issue".into(),
            target_id: Some("#42".into()),
            repo: Some("org/repo".into()),
            pr_number: None,
            status: "idle".into(),
            config_json: None,
            metadata_json: None,
            last_run_at: None,
            next_run_at: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    let got = repos.loops.get_by_id("loop-42").unwrap().unwrap();
    assert_eq!(got.seq, 10);
    assert_eq!(got.status, "idle");
}

#[test]
fn test_loops_allocate_seq() {
    let (repos, _dir) = setup();
    assert_eq!(repos.loops.allocate_seq().unwrap(), 1);
    assert_eq!(repos.loops.allocate_seq().unwrap(), 2);
    assert_eq!(repos.loops.allocate_seq().unwrap(), 3);
}

#[test]
fn test_loops_get_by_seq() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    repos
        .loops
        .upsert(&LoopRecord {
            id: "l-42".into(),
            seq: 42,
            project_id: pid,
            r#type: "worker".into(),
            target_type: "issue".into(),
            target_id: None,
            repo: None,
            pr_number: None,
            status: "running".into(),
            config_json: None,
            metadata_json: None,
            last_run_at: None,
            next_run_at: None,
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    assert_eq!(repos.loops.get_by_seq(42).unwrap().unwrap().id, "l-42");
}

#[test]
fn test_loops_list_by_statuses() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos); // creates "l-1" with status "idle", seq=1
    let t = now();
    // Unique seqs required (UNIQUE constraint); do not rely on REPLACE-by-seq.
    for (seq, s) in [(2i64, "running"), (3, "paused")] {
        repos
            .loops
            .upsert(&LoopRecord {
                id: format!("l-{s}"),
                seq,
                project_id: pid.clone(),
                r#type: "worker".into(),
                target_type: "issue".into(),
                target_id: None,
                repo: None,
                pr_number: None,
                status: s.into(),
                config_json: None,
                metadata_json: None,
                last_run_at: None,
                next_run_at: None,
                created_at: t.clone(),
                updated_at: t.clone(),
            })
            .unwrap();
    }
    // Should find l-1 (idle) + l-paused = 2
    assert_eq!(repos.loops.list_by_statuses(&["idle".into(), "paused".into()]).unwrap().len(), 2);
}

#[test]
fn test_loops_list_by_ids() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    let t = now();
    for i in 0..3 {
        repos
            .loops
            .upsert(&LoopRecord {
                id: format!("l-{i}"),
                seq: i,
                project_id: pid.clone(),
                r#type: "worker".into(),
                target_type: "issue".into(),
                target_id: None,
                repo: None,
                pr_number: None,
                status: "idle".into(),
                config_json: None,
                metadata_json: None,
                last_run_at: None,
                next_run_at: None,
                created_at: t.clone(),
                updated_at: t.clone(),
            })
            .unwrap();
    }
    assert_eq!(repos.loops.list_by_ids(&["l-0".into(), "l-2".into()]).unwrap().len(), 2);
}

#[test]
fn test_loops_terminate_by_project() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos); // creates "l-1" with status "idle"
    let t = now();
    repos
        .loops
        .upsert(&LoopRecord {
            id: "l-active".into(),
            seq: 2,
            project_id: pid.clone(),
            r#type: "worker".into(),
            target_type: "issue".into(),
            target_id: None,
            repo: None,
            pr_number: None,
            status: "idle".into(),
            config_json: None,
            metadata_json: None,
            last_run_at: None,
            next_run_at: None,
            created_at: t.clone(),
            updated_at: t.clone(),
        })
        .unwrap();
    repos
        .loops
        .upsert(&LoopRecord {
            id: "l-done".into(),
            seq: 3,
            project_id: pid.clone(),
            r#type: "worker".into(),
            target_type: "issue".into(),
            target_id: None,
            repo: None,
            pr_number: None,
            status: "completed".into(),
            config_json: None,
            metadata_json: None,
            last_run_at: None,
            next_run_at: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    // Terminate_by_project hits loops NOT in (completed,terminated,stopped).
    // l-1 (idle) + l-active (idle) = 2
    assert_eq!(repos.loops.terminate_by_project(&pid, &now()).unwrap(), 2);
    assert_eq!(repos.loops.get_by_id("l-active").unwrap().unwrap().status, "terminated");
    assert_eq!(repos.loops.get_by_id("l-done").unwrap().unwrap().status, "completed");
    assert_eq!(repos.loops.get_by_id("l-1").unwrap().unwrap().status, "terminated");
}

#[test]
fn test_loops_count_by_type_and_status() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos); // creates "l-1" status="idle" type="worker"
    let t = now();
    // Use explicit IDs to avoid timestamp collision with INSERT OR REPLACE
    repos
        .loops
        .upsert(&LoopRecord {
            id: "l-count-running".into(),
            seq: 10,
            project_id: pid.clone(),
            r#type: "worker".into(),
            target_type: "issue".into(),
            target_id: None,
            repo: None,
            pr_number: None,
            status: "running".into(),
            config_json: None,
            metadata_json: None,
            last_run_at: None,
            next_run_at: None,
            created_at: t.clone(),
            updated_at: t.clone(),
        })
        .unwrap();
    repos
        .loops
        .upsert(&LoopRecord {
            id: "l-count-idle".into(),
            seq: 11,
            project_id: pid,
            r#type: "worker".into(),
            target_type: "issue".into(),
            target_id: None,
            repo: None,
            pr_number: None,
            status: "idle".into(),
            config_json: None,
            metadata_json: None,
            last_run_at: None,
            next_run_at: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    let counts = repos.loops.count_by_type_and_status().unwrap();
    // l-1 (idle worker) + l-count-idle = 2 idle
    assert_eq!(counts.get("worker").and_then(|m| m.get("idle")), Some(&2));
    assert_eq!(counts.get("worker").and_then(|m| m.get("running")), Some(&1));
}

#[test]
fn test_runs_upsert_and_get() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    let t = now();
    repos
        .runs
        .upsert(&RunRecord {
            agent_vendor: None,
            model: None,
            id: "run-1".into(),
            loop_id: lid,
            status: "running".into(),
            current_step: Some("plan".into()),
            last_completed_step: None,
            checkpoint_json: None,
            summary: None,
            error_message: None,
            started_at: t.clone(),
            last_heartbeat_at: Some(t.clone()),
            ended_at: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    let got = repos.runs.get_by_id("run-1").unwrap().unwrap();
    assert_eq!(got.status, "running");
    assert_eq!(got.current_step.as_deref(), Some("plan"));
}

#[test]
fn test_runs_get_latest_by_loop_id() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    let t = now();
    let older = hours_ago(2);
    repos
        .runs
        .upsert(&RunRecord {
            agent_vendor: None,
            model: None,
            id: "run-old".into(),
            loop_id: lid.clone(),
            status: "completed".into(),
            current_step: None,
            last_completed_step: None,
            checkpoint_json: None,
            summary: None,
            error_message: None,
            started_at: older.clone(),
            last_heartbeat_at: None,
            ended_at: Some(older.clone()),
            created_at: older,
            updated_at: t.clone(),
        })
        .unwrap();
    repos
        .runs
        .upsert(&RunRecord {
            agent_vendor: None,
            model: None,
            id: "run-new".into(),
            loop_id: lid,
            status: "running".into(),
            current_step: Some("execute".into()),
            last_completed_step: None,
            checkpoint_json: None,
            summary: None,
            error_message: None,
            started_at: t.clone(),
            last_heartbeat_at: Some(t.clone()),
            ended_at: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    let latest = repos.runs.get_latest_by_loop_id("l-1").unwrap().unwrap();
    assert_eq!(latest.id, "run-new");
}

#[test]
fn test_runs_has_running() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    let t = now();
    repos
        .runs
        .upsert(&RunRecord {
            agent_vendor: None,
            model: None,
            id: "r1".into(),
            loop_id: lid,
            status: "running".into(),
            current_step: None,
            last_completed_step: None,
            checkpoint_json: None,
            summary: None,
            error_message: None,
            started_at: t.clone(),
            last_heartbeat_at: None,
            ended_at: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    assert!(repos.runs.has_running_by_loop_id("l-1").unwrap());
    assert!(!repos.runs.has_running_by_loop_id("l-2").unwrap());
}

#[test]
fn test_runs_list_by_loop() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    for i in 0..3 {
        repos
            .runs
            .upsert(&RunRecord {
                agent_vendor: None,
                model: None,
                id: format!("r-{i}"),
                loop_id: lid.clone(),
                status: "completed".into(),
                current_step: None,
                last_completed_step: None,
                checkpoint_json: None,
                summary: None,
                error_message: None,
                started_at: hours_ago(i),
                last_heartbeat_at: None,
                ended_at: Some(now()),
                created_at: hours_ago(i),
                updated_at: now(),
            })
            .unwrap();
    }
    assert_eq!(repos.runs.list_by_loop("l-1").unwrap().len(), 3);
    assert_eq!(repos.runs.list_by_loop("l-2").unwrap().len(), 0);
}

#[test]
fn test_runs_list_by_status() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    let t = now();
    repos
        .runs
        .upsert(&RunRecord {
            agent_vendor: None,
            model: None,
            id: "r1".into(),
            loop_id: lid,
            status: "failed".into(),
            current_step: None,
            last_completed_step: None,
            checkpoint_json: None,
            summary: None,
            error_message: Some("oops".into()),
            started_at: t.clone(),
            last_heartbeat_at: None,
            ended_at: Some(t.clone()),
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    assert_eq!(repos.runs.list_by_status("failed").unwrap().len(), 1);
    assert!(repos.runs.list_by_status("running").unwrap().is_empty());
}

#[test]
fn test_runs_count_by_status() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    let t = now();
    for s in ["running", "completed"] {
        repos
            .runs
            .upsert(&RunRecord {
                agent_vendor: None,
                model: None,
                id: format!("r-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap()),
                loop_id: lid.clone(),
                status: s.into(),
                current_step: None,
                last_completed_step: None,
                checkpoint_json: None,
                summary: None,
                error_message: None,
                started_at: t.clone(),
                last_heartbeat_at: None,
                ended_at: if s == "completed" { Some(t.clone()) } else { None },
                created_at: t.clone(),
                updated_at: t.clone(),
            })
            .unwrap();
    }
    let counts = repos.runs.count_by_status().unwrap();
    assert_eq!(counts.get("running"), Some(&1));
    assert_eq!(counts.get("completed"), Some(&1));
}

#[test]
fn test_queue_upsert_and_get() {
    let (repos, _dir) = setup();
    repos.queue.upsert(&make_queue_item("q-1", "planner", "dp-1")).unwrap();
    let got = repos.queue.get_by_id("q-1").unwrap().unwrap();
    assert_eq!(got.status, "queued");
    assert_eq!(got.r#type, "planner");
}

#[test]
fn test_queue_create_or_get_active_by_dedupe_inserts() {
    let (repos, _dir) = setup();
    let (created, is_new) =
        repos.queue.create_or_get_active_by_dedupe(&make_queue_item("q-1", "planner", "unique-key")).unwrap();
    assert!(is_new);
    assert_eq!(created.id, "q-1");
}

#[test]
fn test_queue_create_or_get_active_by_dedupe_dedupes() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    // Active dedupe unique index covers planner|reviewer|worker|fixer (V5).
    let input1 = QueueItemRecord {
        loop_id: Some(lid.clone()),
        dedupe_key: "dup-key".into(),
        ..make_queue_item("q-1", "reviewer", "dup-key")
    };
    repos.queue.create_or_get_active_by_dedupe(&input1).unwrap();
    let input2 = QueueItemRecord {
        loop_id: Some(lid),
        dedupe_key: "dup-key".into(),
        ..make_queue_item("q-2", "reviewer", "dup-key")
    };
    let (_, is_new) = repos.queue.create_or_get_active_by_dedupe(&input2).unwrap();
    assert!(!is_new);
}

#[test]
fn test_queue_claim_next() {
    let (repos, _dir) = setup();
    repos.queue.upsert(&make_queue_item("q-1", "planner", "dp-1")).unwrap();
    assert!(repos.queue.claim_next(&now(), "w").unwrap().is_some());
}

#[test]
fn test_queue_claim_next_empty_when_none_queued() {
    let (repos, _dir) = setup();
    assert!(repos.queue.claim_next(&now(), "w").unwrap().is_none());
}

#[test]
fn test_queue_complete() {
    let (repos, _dir) = setup();
    repos.queue.upsert(&make_queue_item("q-1", "planner", "dp-1")).unwrap();
    repos.queue.claim_next(&now(), "w").unwrap();
    repos.queue.complete("q-1", &now()).unwrap();
    assert_eq!(repos.queue.get_by_id("q-1").unwrap().unwrap().status, "completed");
}

#[test]
fn test_queue_mark_retry() {
    let (repos, _dir) = setup();
    repos.queue.upsert(&make_queue_item("q-1", "planner", "dp-1")).unwrap();
    repos
        .queue
        .mark_retry(&QueueMarkRetryInput {
            id: "q-1".into(),
            available_at: now(),
            attempts: 1,
            error_message: Some("transient error".into()),
            error_kind: "retryable_transient".into(),
            updated_at: now(),
        })
        .unwrap();
    let got = repos.queue.get_by_id("q-1").unwrap().unwrap();
    assert_eq!(got.status, "queued");
    assert_eq!(got.attempts, 1);
}

#[test]
fn test_queue_fail() {
    let (repos, _dir) = setup();
    repos.queue.upsert(&make_queue_item("q-1", "planner", "dp-1")).unwrap();
    repos
        .queue
        .fail(&QueueFailInput {
            id: "q-1".into(),
            attempts: 2,
            finished_at: now(),
            error_message: Some("fatal".into()),
            error_kind: "non_retryable".into(),
            updated_at: now(),
        })
        .unwrap();
    assert_eq!(repos.queue.get_by_id("q-1").unwrap().unwrap().status, "manual_intervention");
}

#[test]
fn test_queue_cancel_by_loop() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    repos
        .queue
        .upsert(&QueueItemRecord {
            project_id: None,
            loop_id: Some(lid.clone()),
            ..make_queue_item("q-1", "planner", "dp-1")
        })
        .unwrap();
    repos.queue.cancel_by_loop(&lid, &now(), Some("cancel")).unwrap();
    assert_eq!(repos.queue.get_by_id("q-1").unwrap().unwrap().status, "cancelled");
}

#[test]
fn test_queue_list_scheduled_and_stats() {
    let (repos, _dir) = setup();
    insert_project_and_loop(&repos);
    repos.queue.upsert(&make_queue_item("q-1", "planner", "dp-1")).unwrap();
    assert_eq!(repos.queue.list_scheduled(&now(), 10).unwrap().len(), 1);
    let stats = repos.queue.stats(&now()).unwrap();
    assert_eq!(stats.total_queued, 1);
    assert_eq!(stats.eligible_queued, 1);
}

#[test]
fn test_queue_list_queued_with_limit() {
    let (repos, _dir) = setup();
    for i in 0..5 {
        repos.queue.upsert(&make_queue_item(&format!("q-{i}"), "planner", &format!("dp-{i}"))).unwrap();
    }
    assert_eq!(repos.queue.list_queued(3).unwrap().len(), 3);
}

#[test]
fn test_queue_count_by_status() {
    let (repos, _dir) = setup();
    repos.queue.upsert(&make_queue_item("q-1", "planner", "dp-1")).unwrap();
    assert_eq!(repos.queue.count_by_status("queued").unwrap(), 1);
    assert_eq!(repos.queue.count_by_status("running").unwrap(), 0);
}

#[test]
fn test_queue_count_active_by_loop_id() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    repos
        .queue
        .upsert(&QueueItemRecord { loop_id: Some(lid.clone()), ..make_queue_item("q-1", "planner", "dp-1") })
        .unwrap();
    repos.queue.upsert(&QueueItemRecord { loop_id: Some(lid), ..make_queue_item("q-2", "reviewer", "dp-2") }).unwrap();
    assert_eq!(repos.queue.count_active_by_loop_id("l-1").unwrap(), 2);
}

#[test]
fn test_queue_cancel_by_project() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    repos
        .queue
        .upsert(&QueueItemRecord { project_id: Some(pid), loop_id: None, ..make_queue_item("q-1", "planner", "dp-1") })
        .unwrap();
    repos.queue.cancel_by_project("p-1", &now(), Some("shutdown")).unwrap();
    assert_eq!(repos.queue.get_by_id("q-1").unwrap().unwrap().status, "cancelled");
}

#[test]
fn test_queue_requeue_running_by_loop() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    repos
        .queue
        .upsert(&QueueItemRecord { loop_id: Some(lid.clone()), ..make_queue_item("q-1", "planner", "dp-1") })
        .unwrap();
    repos.queue.claim_next(&now(), "w").unwrap();
    repos.queue.requeue_running_by_loop(&lid, &now()).unwrap();
    assert_eq!(repos.queue.get_by_id("q-1").unwrap().unwrap().status, "queued");
}

#[test]
fn test_queue_update_lock_key() {
    let (repos, _dir) = setup();
    repos.queue.upsert(&make_queue_item("q-1", "planner", "dp-1")).unwrap();
    repos.queue.update_lock_key("q-1", "lk-1", &now()).unwrap();
    assert_eq!(repos.queue.get_by_id("q-1").unwrap().unwrap().lock_key.unwrap(), "lk-1");
}

#[test]
fn test_queue_claim_next_of_type() {
    let (repos, _dir) = setup();
    repos.queue.upsert(&make_queue_item("q-1", "planner", "dp-1")).unwrap();
    repos.queue.upsert(&make_queue_item("q-2", "reviewer", "dp-2")).unwrap();
    let planner = repos.queue.claim_next_of_type(&now(), "w", "planner").unwrap();
    assert_eq!(planner.unwrap().id, "q-1");
}

#[test]
fn test_locks_acquire_and_get() {
    let (repos, _dir) = setup();
    let lock = LockRecord {
        key: "lk-1".into(),
        owner: "w".into(),
        reason: None,
        expires_at: hours_ago(-1),
        created_at: now(),
        updated_at: now(),
    };
    assert!(repos.locks.acquire(&lock).unwrap());
    assert_eq!(repos.locks.get("lk-1").unwrap().unwrap().owner, "w");
}

#[test]
fn test_locks_acquire_conflict() {
    let (repos, _dir) = setup();
    // First acquire: lock that expires in the future
    let future_lock = LockRecord {
        key: "lk-1".into(),
        owner: "w1".into(),
        reason: None,
        expires_at: hours_ago(-1),
        created_at: now(),
        updated_at: now(),
    };
    assert!(repos.locks.acquire(&future_lock).unwrap());
    // Second acquire: same key, but with a past expiry.
    // The WHERE clause (old.expires_at <= new.expires_at) fails because
    // the existing lock still has a future expiry > the new past expiry.
    let past_lock = LockRecord {
        key: "lk-1".into(),
        owner: "w2".into(),
        reason: None,
        expires_at: hours_ago(1),
        created_at: now(),
        updated_at: now(),
    };
    assert!(!repos.locks.acquire(&past_lock).unwrap());
}

#[test]
fn test_locks_acquire_expired_can_be_replaced() {
    let (repos, _dir) = setup();
    repos
        .locks
        .acquire(&LockRecord {
            key: "lk-y".into(),
            owner: "stale".into(),
            reason: None,
            expires_at: hours_ago(2),
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    assert!(repos
        .locks
        .acquire(&LockRecord {
            key: "lk-y".into(),
            owner: "new-owner".into(),
            reason: None,
            expires_at: hours_ago(-1),
            created_at: now(),
            updated_at: now(),
        })
        .unwrap());
    assert_eq!(repos.locks.get("lk-y").unwrap().unwrap().owner, "new-owner");
}

#[test]
fn test_locks_release() {
    let (repos, _dir) = setup();
    repos
        .locks
        .acquire(&LockRecord {
            key: "lk-1".into(),
            owner: "w".into(),
            reason: None,
            expires_at: hours_ago(-1),
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    repos.locks.release("lk-1").unwrap();
    assert!(repos.locks.get("lk-1").unwrap().is_none());
}

#[test]
fn test_locks_refresh() {
    let (repos, _dir) = setup();
    repos
        .locks
        .acquire(&LockRecord {
            key: "lk-1".into(),
            owner: "w".into(),
            reason: None,
            expires_at: hours_ago(-1),
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    assert!(repos
        .locks
        .refresh(&LockRecord {
            key: "lk-1".into(),
            owner: "w".into(),
            reason: Some("still working".into()),
            expires_at: hours_ago(-2),
            created_at: now(),
            updated_at: now(),
        })
        .unwrap());
}

#[test]
fn test_locks_list_expired() {
    let (repos, _dir) = setup();
    repos
        .locks
        .acquire(&LockRecord {
            key: "expired".into(),
            owner: "w".into(),
            reason: None,
            expires_at: hours_ago(1),
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    repos
        .locks
        .acquire(&LockRecord {
            key: "active".into(),
            owner: "w".into(),
            reason: None,
            expires_at: hours_ago(-1),
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    let expired = repos.locks.list_expired(&now()).unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].key, "expired");
}

#[test]
fn test_locks_list_active() {
    let (repos, _dir) = setup();
    repos
        .locks
        .acquire(&LockRecord {
            key: "expired".into(),
            owner: "w".into(),
            reason: None,
            expires_at: hours_ago(1),
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    repos
        .locks
        .acquire(&LockRecord {
            key: "active".into(),
            owner: "w".into(),
            reason: None,
            expires_at: hours_ago(-1),
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    let active = repos.locks.list_active(&now()).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].key, "active");
}

#[test]
fn test_locks_get_missing() {
    let (repos, _dir) = setup();
    assert!(repos.locks.get("no-such").unwrap().is_none());
}

#[test]
fn test_events_append_and_list() {
    let (repos, _dir) = setup();
    repos
        .events
        .append(&EventLogRecord {
            id: "evt-1".into(),
            event_type: "test.event".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            correlation_id: None,
            causation_id: None,
            actor_type: None,
            actor_id: None,
            actor_display_name: None,
            payload_json: "{}".into(),
            created_at: now(),
        })
        .unwrap();
    let list = repos.events.list(10).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].event_type, "test.event");
}

#[test]
fn test_events_list_since() {
    let (repos, _dir) = setup();
    let old = hours_ago(3);
    repos
        .events
        .append(&EventLogRecord {
            id: "evt-old".into(),
            event_type: "old".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            correlation_id: None,
            causation_id: None,
            actor_type: None,
            actor_id: None,
            actor_display_name: None,
            payload_json: "{}".into(),
            created_at: old,
        })
        .unwrap();
    repos
        .events
        .append(&EventLogRecord {
            id: "evt-new".into(),
            event_type: "new".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            correlation_id: None,
            causation_id: None,
            actor_type: None,
            actor_id: None,
            actor_display_name: None,
            payload_json: "{}".into(),
            created_at: now(),
        })
        .unwrap();
    let list = repos.events.list_since(&hours_ago(1)).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].event_type, "new");
}

#[test]
fn test_events_list_by_entity() {
    let (repos, _dir) = setup();
    repos
        .events
        .append(&EventLogRecord {
            id: "evt-1".into(),
            event_type: "created".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            entity_type: Some("project".into()),
            entity_id: Some("p-1".into()),
            correlation_id: None,
            causation_id: None,
            actor_type: None,
            actor_id: None,
            actor_display_name: None,
            payload_json: "{}".into(),
            created_at: now(),
        })
        .unwrap();
    let list = repos.events.list_by_entity("project", "p-1").unwrap();
    assert_eq!(list.len(), 1);
}

#[test]
fn test_eventlog_emit() {
    let (repos, _dir) = setup();
    let log = crate::EventLog::new(repos.events);
    let record =
        log.emit(&AppendInput { event_type: "test.event".into(), project_id: None, ..AppendInput::new("") }).unwrap();
    assert_eq!(record.event_type, "test.event");
    assert_eq!(record.actor_type.as_deref(), Some("system"));
    assert!(record.id.starts_with("event_"));
}

#[test]
fn test_eventlog_emit_with_explicit_id() {
    let (repos, _dir) = setup();
    let log = crate::EventLog::new(repos.events);
    let record = log
        .emit(&AppendInput {
            event_type: "custom.event".into(),
            id: Some("my-custom-id".into()),
            created_at: Some("2024-01-01T00:00:00.000Z".into()),
            ..AppendInput::new("")
        })
        .unwrap();
    assert_eq!(record.id, "my-custom-id");
    assert_eq!(record.created_at, "2024-01-01T00:00:00.000Z");
}

#[test]
fn test_notifications_upsert_and_get() {
    let (repos, _dir) = setup();
    let t = now();
    repos
        .notifications
        .upsert(&NotificationRecord {
            id: "n-1".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            channel: "log".into(),
            level: "info".into(),
            title: "Test".into(),
            subtitle: None,
            body: "Body".into(),
            status: "pending".into(),
            dedupe_key: Some("dk-1".into()),
            error_message: None,
            payload_json: None,
            sent_at: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    let got = repos.notifications.get_by_id("n-1").unwrap().unwrap();
    assert_eq!(got.title, "Test");
}

#[test]
fn test_notifications_list() {
    let (repos, _dir) = setup();
    let t = now();
    for i in 0..3 {
        repos
            .notifications
            .upsert(&NotificationRecord {
                id: format!("n-{i}"),
                project_id: None,
                loop_id: None,
                run_id: None,
                entity_type: None,
                entity_id: None,
                channel: "log".into(),
                level: "info".into(),
                title: format!("notif {i}"),
                subtitle: None,
                body: "".into(),
                status: "pending".into(),
                dedupe_key: None,
                error_message: None,
                payload_json: None,
                sent_at: None,
                created_at: t.clone(),
                updated_at: t.clone(),
            })
            .unwrap();
    }
    assert_eq!(repos.notifications.list(10).unwrap().len(), 3);
    assert_eq!(repos.notifications.list(2).unwrap().len(), 2);
}

#[test]
fn test_notifications_get_latest_by_dedupe() {
    let (repos, _dir) = setup();
    let t = now();
    repos
        .notifications
        .upsert(&NotificationRecord {
            id: "n-1".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            channel: "log".into(),
            level: "info".into(),
            title: "first".into(),
            subtitle: None,
            body: "".into(),
            status: "sent".into(),
            dedupe_key: Some("dk-test".into()),
            error_message: None,
            payload_json: None,
            sent_at: Some(hours_ago(2)),
            created_at: hours_ago(2),
            updated_at: hours_ago(2),
        })
        .unwrap();
    repos
        .notifications
        .upsert(&NotificationRecord {
            id: "n-2".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            channel: "log".into(),
            level: "info".into(),
            title: "second".into(),
            subtitle: None,
            body: "".into(),
            status: "pending".into(),
            dedupe_key: Some("dk-test".into()),
            error_message: None,
            payload_json: None,
            sent_at: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    let latest = repos.notifications.get_latest_by_dedupe("log", "dk-test").unwrap().unwrap();
    assert_eq!(latest.title, "second");
}

#[test]
fn test_worktrees_upsert_and_get() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    repos
        .worktrees
        .upsert(&WorktreeRecord {
            id: "wt-1".into(),
            project_id: pid,
            repo_path: "/tmp/r".into(),
            worktree_path: "/tmp/r-wt".into(),
            branch: "feature-x".into(),
            base_branch: Some("main".into()),
            status: "active".into(),
            head_sha: Some("abc123".into()),
            metadata_json: None,
            created_at: now(),
            updated_at: now(),
            cleaned_at: None,
        })
        .unwrap();
    let got = repos.worktrees.get_by_id("wt-1").unwrap().unwrap();
    assert_eq!(got.branch, "feature-x");
    assert_eq!(got.status, "active");
}

#[test]
fn test_worktrees_get_by_branch() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    repos
        .worktrees
        .upsert(&WorktreeRecord {
            id: "wt-1".into(),
            project_id: pid.clone(),
            repo_path: "/tmp/r".into(),
            worktree_path: "/tmp/r-wt".into(),
            branch: "my-branch".into(),
            base_branch: None,
            status: "active".into(),
            head_sha: None,
            metadata_json: None,
            created_at: now(),
            updated_at: now(),
            cleaned_at: None,
        })
        .unwrap();
    let got = repos.worktrees.get_by_branch(&pid, "my-branch").unwrap().unwrap();
    assert_eq!(got.id, "wt-1");
}

#[test]
fn test_worktrees_list_by_project() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    for i in 0..2 {
        repos
            .worktrees
            .upsert(&WorktreeRecord {
                id: format!("wt-{i}"),
                project_id: pid.clone(),
                repo_path: "/tmp/r".into(),
                worktree_path: format!("/tmp/r-{i}"),
                branch: format!("br-{i}"),
                base_branch: None,
                status: "active".into(),
                head_sha: None,
                metadata_json: None,
                created_at: now(),
                updated_at: now(),
                cleaned_at: None,
            })
            .unwrap();
    }
    assert_eq!(repos.worktrees.list_by_project(&pid).unwrap().len(), 2);
    assert_eq!(repos.worktrees.list_by_project("p-2").unwrap().len(), 0);
}

#[test]
fn test_worktrees_list_cleanup_candidates() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    repos
        .worktrees
        .upsert(&WorktreeRecord {
            id: "wt-cleanable".into(),
            project_id: pid.clone(),
            repo_path: "/tmp/r".into(),
            worktree_path: "/tmp/r-clean".into(),
            branch: "done-br".into(),
            base_branch: None,
            status: "abandoned".into(),
            head_sha: None,
            metadata_json: None,
            created_at: hours_ago(5),
            updated_at: hours_ago(5),
            cleaned_at: None,
        })
        .unwrap();
    repos
        .worktrees
        .upsert(&WorktreeRecord {
            id: "wt-active".into(),
            project_id: pid,
            repo_path: "/tmp/r".into(),
            worktree_path: "/tmp/r-active".into(),
            branch: "active-br".into(),
            base_branch: None,
            status: "active".into(),
            head_sha: None,
            metadata_json: None,
            created_at: now(),
            updated_at: now(),
            cleaned_at: None,
        })
        .unwrap();
    let candidates = repos.worktrees.list_cleanup_candidates(10).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "wt-cleanable");
}

#[test]
fn test_worktrees_list_active() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    repos
        .worktrees
        .upsert(&WorktreeRecord {
            id: "wt-1".into(),
            project_id: pid.clone(),
            repo_path: "/tmp/r".into(),
            worktree_path: "/tmp/r-1".into(),
            branch: "active".into(),
            base_branch: None,
            status: "active".into(),
            head_sha: None,
            metadata_json: None,
            created_at: now(),
            updated_at: now(),
            cleaned_at: None,
        })
        .unwrap();
    repos
        .worktrees
        .upsert(&WorktreeRecord {
            id: "wt-2".into(),
            project_id: pid,
            repo_path: "/tmp/r".into(),
            worktree_path: "/tmp/r-2".into(),
            branch: "stale".into(),
            base_branch: None,
            status: "abandoned".into(),
            head_sha: None,
            metadata_json: None,
            created_at: now(),
            updated_at: now(),
            cleaned_at: None,
        })
        .unwrap();
    assert_eq!(repos.worktrees.list_active().unwrap().len(), 1);
}

#[test]
fn test_worktrees_get_latest_by_loop_id() {
    let (repos, _dir) = setup();
    let (pid, lid) = insert_project_and_loop(&repos);
    let t = now();
    repos
        .worktrees
        .upsert(&WorktreeRecord {
            id: "wt-other".into(),
            project_id: pid.clone(),
            repo_path: "/tmp/r".into(),
            worktree_path: "/tmp/other".into(),
            branch: "planner/other-loop".into(),
            base_branch: None,
            status: "active".into(),
            head_sha: None,
            metadata_json: Some(r#"{"loop_id":"other-loop"}"#.into()),
            created_at: t.clone(),
            updated_at: t.clone(),
            cleaned_at: None,
        })
        .unwrap();
    repos
        .worktrees
        .upsert(&WorktreeRecord {
            id: "wt-loop".into(),
            project_id: pid,
            repo_path: "/tmp/r".into(),
            worktree_path: "/tmp/loop-wt".into(),
            branch: format!("planner/{lid}"),
            base_branch: None,
            status: "created".into(),
            head_sha: None,
            metadata_json: Some(format!(r#"{{"loop_id":"{lid}","step":"prepare-worktree"}}"#)),
            created_at: t.clone(),
            updated_at: t,
            cleaned_at: None,
        })
        .unwrap();

    let got = repos.worktrees.get_latest_by_loop_id(&lid).unwrap().unwrap();
    assert_eq!(got.id, "wt-loop");
    assert_eq!(got.worktree_path, "/tmp/loop-wt");
    assert!(repos.worktrees.get_latest_by_loop_id("missing").unwrap().is_none());
}

#[test]
fn test_worktrees_touch_cleanup_attempt() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    repos
        .worktrees
        .upsert(&WorktreeRecord {
            id: "wt-1".into(),
            project_id: pid,
            repo_path: "/tmp/r".into(),
            worktree_path: "/tmp/r-wt".into(),
            branch: "br".into(),
            base_branch: None,
            status: "abandoned".into(),
            head_sha: None,
            metadata_json: None,
            created_at: now(),
            updated_at: now(),
            cleaned_at: None,
        })
        .unwrap();
    repos.worktrees.touch_cleanup_attempt("wt-1", &now()).unwrap();
}

#[test]
fn test_agent_executions_upsert_and_get() {
    let (repos, _dir) = setup();
    let t = now();
    repos
        .agent_executions
        .upsert(&AgentExecutionRecord {
            id: "ae-1".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            vendor: "claude".into(),
            status: "running".into(),
            pid: Some(12345),
            command_json: Some(r#"["arg1"]"#.into()),
            cwd: Some("/tmp".into()),
            summary: None,
            parse_status: None,
            completion_signal: None,
            heartbeat_count: 0,
            last_heartbeat_at: Some(t.clone()),
            output_json: None,
            error_message: None,
            native_session_id: None,
            native_resume_mode: None,
            native_resume_status: None,
            native_resume_error: None,
            started_at: t.clone(),
            ended_at: None,
            metadata_json: Some(r#"{"process_group":12345}"#.into()),
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    let got = repos.agent_executions.get_by_id("ae-1").unwrap().unwrap();
    assert_eq!(got.vendor, "claude");
    assert_eq!(got.status, "running");
}

#[test]
fn test_agent_executions_update_terminal_preserves_identity() {
    let (repos, _dir) = setup();
    let t = now();
    repos
        .agent_executions
        .upsert(&AgentExecutionRecord {
            id: "ae-term".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            vendor: "claude-code".into(),
            status: "running".into(),
            pid: Some(99999),
            command_json: Some(r#"{"command":"claude"}"#.into()),
            cwd: Some("/tmp/worktree".into()),
            summary: None,
            parse_status: None,
            completion_signal: None,
            heartbeat_count: 0,
            last_heartbeat_at: None,
            output_json: Some(r#"{"stdoutLogPath":"/tmp/a.log"}"#.into()),
            error_message: None,
            native_session_id: None,
            native_resume_mode: Some("checkpoint_restart".into()),
            native_resume_status: Some("unavailable".into()),
            native_resume_error: None,
            started_at: t.clone(),
            ended_at: None,
            metadata_json: Some(r#"{"process_group":99999,"pid":99999}"#.into()),
            created_at: t.clone(),
            updated_at: t.clone(),
        })
        .unwrap();

    let ended = now();
    repos
        .agent_executions
        .update_terminal(
            "ae-term",
            "completed",
            Some("wrote the file"),
            Some("parsed"),
            Some("__LOOPER_RESULT__="),
            None,
            Some("sess-abc"),
            7,
            Some(&ended),
            &ended,
            &ended,
        )
        .unwrap();

    let got = repos.agent_executions.get_by_id("ae-term").unwrap().unwrap();
    assert_eq!(got.status, "completed");
    assert_eq!(got.summary.as_deref(), Some("wrote the file"));
    assert_eq!(got.heartbeat_count, 7);
    assert_eq!(got.native_session_id.as_deref(), Some("sess-abc"));
    assert_eq!(got.ended_at.as_deref(), Some(ended.as_str()));
    // Identity columns must survive terminal update (no INSERT OR REPLACE wipe)
    assert_eq!(got.vendor, "claude-code");
    assert_eq!(got.pid, Some(99999));
    assert_eq!(got.cwd.as_deref(), Some("/tmp/worktree"));
    assert!(got.metadata_json.as_deref().unwrap().contains("99999"));
    assert!(got.output_json.as_deref().unwrap().contains("stdoutLogPath"));
    // No longer active
    assert!(repos.agent_executions.list_active().unwrap().is_empty());
}

#[test]
fn test_agent_executions_update_status_cancelling() {
    let (repos, _dir) = setup();
    let t = now();
    repos
        .agent_executions
        .upsert(&AgentExecutionRecord {
            id: "ae-kill".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            vendor: "claude".into(),
            status: "running".into(),
            pid: Some(1),
            command_json: None,
            cwd: Some("/tmp".into()),
            summary: None,
            parse_status: None,
            completion_signal: None,
            heartbeat_count: 0,
            last_heartbeat_at: None,
            output_json: None,
            error_message: None,
            native_session_id: None,
            native_resume_mode: None,
            native_resume_status: None,
            native_resume_error: None,
            started_at: t.clone(),
            ended_at: None,
            metadata_json: None,
            created_at: t.clone(),
            updated_at: t.clone(),
        })
        .unwrap();
    let t2 = now();
    repos.agent_executions.update_status("ae-kill", "cancelling", &t2).unwrap();
    let got = repos.agent_executions.get_by_id("ae-kill").unwrap().unwrap();
    assert_eq!(got.status, "cancelling");
    assert_eq!(got.cwd.as_deref(), Some("/tmp"));
    assert_eq!(got.pid, Some(1));
}

#[test]
fn test_agent_executions_get_latest_by_run_id() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    // Need a run record to satisfy FK: agent_executions.run_id → runs.id
    repos
        .runs
        .upsert(&RunRecord {
            agent_vendor: None,
            model: None,
            id: "r-1".into(),
            loop_id: lid,
            status: "running".into(),
            current_step: None,
            last_completed_step: None,
            checkpoint_json: None,
            summary: None,
            error_message: None,
            started_at: now(),
            last_heartbeat_at: None,
            ended_at: None,
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    let t = now();
    repos
        .agent_executions
        .upsert(&AgentExecutionRecord {
            id: "ae-1".into(),
            project_id: None,
            loop_id: None,
            run_id: Some("r-1".into()),
            vendor: "codex".into(),
            status: "completed".into(),
            pid: None,
            command_json: None,
            cwd: None,
            summary: Some("done".into()),
            parse_status: None,
            completion_signal: None,
            heartbeat_count: 0,
            last_heartbeat_at: None,
            output_json: None,
            error_message: None,
            native_session_id: None,
            native_resume_mode: None,
            native_resume_status: None,
            native_resume_error: None,
            started_at: hours_ago(1),
            ended_at: Some(t.clone()),
            metadata_json: None,
            created_at: hours_ago(1),
            updated_at: t.clone(),
        })
        .unwrap();
    repos
        .agent_executions
        .upsert(&AgentExecutionRecord {
            id: "ae-2".into(),
            project_id: None,
            loop_id: None,
            run_id: Some("r-1".into()),
            vendor: "opencode".into(),
            status: "running".into(),
            pid: Some(9999),
            command_json: None,
            cwd: None,
            summary: None,
            parse_status: None,
            completion_signal: None,
            heartbeat_count: 0,
            last_heartbeat_at: Some(t.clone()),
            output_json: None,
            error_message: None,
            native_session_id: None,
            native_resume_mode: None,
            native_resume_status: None,
            native_resume_error: None,
            started_at: t.clone(),
            ended_at: None,
            metadata_json: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    let latest = repos.agent_executions.get_latest_by_run_id("r-1").unwrap().unwrap();
    assert_eq!(latest.id, "ae-2");
}

#[test]
fn test_agent_executions_get_latest_active_by_run_id() {
    let (repos, _dir) = setup();
    let (_, lid) = insert_project_and_loop(&repos);
    // Need a run record to satisfy FK: agent_executions.run_id → runs.id
    repos
        .runs
        .upsert(&RunRecord {
            agent_vendor: None,
            model: None,
            id: "r-1".into(),
            loop_id: lid,
            status: "running".into(),
            current_step: None,
            last_completed_step: None,
            checkpoint_json: None,
            summary: None,
            error_message: None,
            started_at: now(),
            last_heartbeat_at: None,
            ended_at: None,
            created_at: now(),
            updated_at: now(),
        })
        .unwrap();
    let t = now();
    repos
        .agent_executions
        .upsert(&AgentExecutionRecord {
            id: "ae-1".into(),
            project_id: None,
            loop_id: None,
            run_id: Some("r-1".into()),
            vendor: "codex".into(),
            status: "completed".into(),
            pid: None,
            command_json: None,
            cwd: None,
            summary: None,
            parse_status: None,
            completion_signal: None,
            heartbeat_count: 0,
            last_heartbeat_at: None,
            output_json: None,
            error_message: None,
            native_session_id: None,
            native_resume_mode: None,
            native_resume_status: None,
            native_resume_error: None,
            started_at: hours_ago(1),
            ended_at: Some(t.clone()),
            metadata_json: None,
            created_at: hours_ago(1),
            updated_at: t.clone(),
        })
        .unwrap();
    assert!(repos.agent_executions.get_latest_active_by_run_id("r-1").unwrap().is_none());
    repos
        .agent_executions
        .upsert(&AgentExecutionRecord {
            id: "ae-2".into(),
            project_id: None,
            loop_id: None,
            run_id: Some("r-1".into()),
            vendor: "opencode".into(),
            status: "running".into(),
            pid: Some(9999),
            command_json: None,
            cwd: None,
            summary: None,
            parse_status: None,
            completion_signal: None,
            heartbeat_count: 0,
            last_heartbeat_at: Some(t.clone()),
            output_json: None,
            error_message: None,
            native_session_id: None,
            native_resume_mode: None,
            native_resume_status: None,
            native_resume_error: None,
            started_at: t.clone(),
            ended_at: None,
            metadata_json: None,
            created_at: t.clone(),
            updated_at: t,
        })
        .unwrap();
    let active = repos.agent_executions.get_latest_active_by_run_id("r-1").unwrap().unwrap();
    assert_eq!(active.id, "ae-2");
}

#[test]
fn test_agent_executions_list_active() {
    let (repos, _dir) = setup();
    let t = now();
    repos
        .agent_executions
        .upsert(&AgentExecutionRecord {
            id: "running".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            vendor: "claude".into(),
            status: "running".into(),
            pid: Some(1),
            command_json: None,
            cwd: None,
            summary: None,
            parse_status: None,
            completion_signal: None,
            heartbeat_count: 0,
            last_heartbeat_at: Some(t.clone()),
            output_json: None,
            error_message: None,
            native_session_id: None,
            native_resume_mode: None,
            native_resume_status: None,
            native_resume_error: None,
            started_at: t.clone(),
            ended_at: None,
            metadata_json: None,
            created_at: t.clone(),
            updated_at: t.clone(),
        })
        .unwrap();
    repos
        .agent_executions
        .upsert(&AgentExecutionRecord {
            id: "completed".into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            vendor: "claude".into(),
            status: "completed".into(),
            pid: None,
            command_json: None,
            cwd: None,
            summary: None,
            parse_status: None,
            completion_signal: None,
            heartbeat_count: 0,
            last_heartbeat_at: None,
            output_json: None,
            error_message: None,
            native_session_id: None,
            native_resume_mode: None,
            native_resume_status: None,
            native_resume_error: None,
            started_at: hours_ago(2),
            ended_at: Some(t.clone()),
            metadata_json: None,
            created_at: hours_ago(2),
            updated_at: t,
        })
        .unwrap();
    assert_eq!(repos.agent_executions.list_active().unwrap().len(), 1);
}

#[test]
fn test_pr_snapshots_upsert_and_list() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    repos
        .pull_request_snapshots
        .upsert(&PullRequestSnapshotRecord {
            id: "prs-1".into(),
            project_id: pid,
            repo: "org/repo".into(),
            pr_number: 42,
            head_sha: "abc".into(),
            base_sha: Some("def".into()),
            title: Some("My PR".into()),
            body: Some("Body".into()),
            author: Some("user1".into()),
            diff_ref: None,
            checks_summary: None,
            unresolved_thread_count: Some(0),
            review_state: Some("approved".into()),
            payload_json: None,
            captured_at: now(),
            created_at: now(),
        })
        .unwrap();
    assert_eq!(repos.pull_request_snapshots.list().unwrap().len(), 1);
}

#[test]
fn test_pr_snapshots_get_latest() {
    let (repos, _dir) = setup();
    let (pid, _) = insert_project_and_loop(&repos);
    let t = now();
    repos
        .pull_request_snapshots
        .upsert(&PullRequestSnapshotRecord {
            id: "prs-old".into(),
            project_id: pid.clone(),
            repo: "org/repo".into(),
            pr_number: 42,
            head_sha: "old-sha".into(),
            base_sha: None,
            title: None,
            body: None,
            author: None,
            diff_ref: None,
            checks_summary: None,
            unresolved_thread_count: None,
            review_state: None,
            payload_json: None,
            captured_at: hours_ago(1),
            created_at: hours_ago(1),
        })
        .unwrap();
    repos
        .pull_request_snapshots
        .upsert(&PullRequestSnapshotRecord {
            id: "prs-new".into(),
            project_id: pid,
            repo: "org/repo".into(),
            pr_number: 42,
            head_sha: "new-sha".into(),
            base_sha: None,
            title: None,
            body: None,
            author: None,
            diff_ref: None,
            checks_summary: None,
            unresolved_thread_count: None,
            review_state: None,
            payload_json: None,
            captured_at: t.clone(),
            created_at: t,
        })
        .unwrap();
    let latest = repos.pull_request_snapshots.get_latest("org/repo", 42).unwrap().unwrap();
    assert_eq!(latest.head_sha, "new-sha");
}

#[test]
fn test_webhook_forwarders_upsert_list_delete() {
    let (repos, _dir) = setup();
    let t = 1_700_000_000i64;
    repos
        .webhook_forwarders
        .upsert(&WebhookForwarderRecord {
            repo: "org/repo".into(),
            pid: 12345,
            process_start: t,
            fingerprint: "fp1".into(),
            endpoint: "http://localhost:9999".into(),
            events: "push,pull_request".into(),
            gh_path: "/usr/local/bin/gh".into(),
            daemon_id: "daemon-1".into(),
            spawned_at: t,
            updated_at: t,
        })
        .unwrap();
    assert_eq!(repos.webhook_forwarders.list().unwrap().len(), 1);
    repos.webhook_forwarders.delete("org/repo").unwrap();
    assert!(repos.webhook_forwarders.list().unwrap().is_empty());
}

#[test]
fn test_webhook_tunnel_hooks_upsert_get() {
    let (repos, _dir) = setup();
    let t = 1_700_000_000i64;
    repos
        .webhook_tunnel_hooks
        .upsert(&WebhookTunnelHookRecord {
            repo: "org/repo".into(),
            hook_id: 999,
            managed_url: "https://tunnel.example.com/hook".into(),
            secret_ref: "sec-1".into(),
            last_ping_at: None,
            consecutive_disables: 0,
            last_disable_at: None,
            orphaned: false,
            created_at: t,
            updated_at: t,
        })
        .unwrap();
    let (got, found) = repos.webhook_tunnel_hooks.get("org/repo").unwrap();
    assert!(found);
    assert_eq!(got.unwrap().hook_id, 999);
}

#[test]
fn test_webhook_tunnel_hooks_get_missing() {
    let (repos, _dir) = setup();
    assert!(!repos.webhook_tunnel_hooks.get("no-such").unwrap().1);
}

#[test]
fn test_webhook_tunnel_hooks_mark_orphaned() {
    let (repos, _dir) = setup();
    let t = 1_700_000_000i64;
    repos
        .webhook_tunnel_hooks
        .upsert(&WebhookTunnelHookRecord {
            repo: "org/repo".into(),
            hook_id: 1,
            managed_url: "url".into(),
            secret_ref: "sec".into(),
            last_ping_at: None,
            consecutive_disables: 0,
            last_disable_at: None,
            orphaned: false,
            created_at: t,
            updated_at: t,
        })
        .unwrap();
    repos.webhook_tunnel_hooks.mark_orphaned("org/repo", true, t + 100).unwrap();
    let (got, _) = repos.webhook_tunnel_hooks.get("org/repo").unwrap();
    assert!(got.unwrap().orphaned);
}

#[test]
fn test_webhook_tunnel_hooks_update_ping() {
    let (repos, _dir) = setup();
    let t = 1_700_000_000i64;
    repos
        .webhook_tunnel_hooks
        .upsert(&WebhookTunnelHookRecord {
            repo: "org/repo".into(),
            hook_id: 1,
            managed_url: "url".into(),
            secret_ref: "sec".into(),
            last_ping_at: None,
            consecutive_disables: 0,
            last_disable_at: None,
            orphaned: false,
            created_at: t,
            updated_at: t,
        })
        .unwrap();
    repos.webhook_tunnel_hooks.update_ping("org/repo", t + 500).unwrap();
    let (got, _) = repos.webhook_tunnel_hooks.get("org/repo").unwrap();
    assert_eq!(got.unwrap().last_ping_at, Some(t + 500));
}

#[test]
fn test_nullable_string() {
    assert_eq!(nullable_string("".into()), None);
    assert_eq!(nullable_string("hello".into()), Some("hello".into()));
}

#[test]
fn test_bool_to_int_and_back() {
    assert_eq!(bool_to_int(true), 1);
    assert_eq!(bool_to_int(false), 0);
    assert!(int_to_bool(1));
    assert!(!int_to_bool(0));
}

#[test]
fn test_sql_placeholders() {
    assert_eq!(sql_placeholders(0), "");
    assert_eq!(sql_placeholders(1), "?");
    assert_eq!(sql_placeholders(3), "?,?,?");
}

#[test]
fn test_chunk_strings() {
    let input = vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()];
    let chunks = chunk_strings(&input, 2);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].len(), 2);
    assert_eq!(chunks[2].len(), 1);
}

#[test]
fn test_empty_list_by_statuses() {
    let (repos, _dir) = setup();
    assert!(repos.loops.list_by_statuses(&[]).unwrap().is_empty());
    assert!(repos.queue.list_by_statuses(&[]).unwrap().is_empty());
}
