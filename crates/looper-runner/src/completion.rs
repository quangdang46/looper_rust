//! Shared terminal-state helpers for role runners.

use chrono::Utc;
use looper_scheduler::scheduler::SendRepos;
use std::sync::Arc;

/// Mark a queue item terminal (`completed` / `failed` / `cancelled`).
pub fn mark_queue_terminal(repos: &Arc<SendRepos>, item_id: &str, status: &str, last_error: Option<String>) {
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let Ok(guard) = repos.0.lock() else {
        tracing::warn!("mark_queue_terminal: lock poisoned (item={item_id})");
        return;
    };
    match guard.queue.get_by_id(item_id) {
        Ok(Some(mut qi)) => {
            // Don't clobber a terminal status with a weaker one.
            if matches!(qi.status.as_str(), "completed" | "cancelled" | "failed" | "manual_intervention")
                && status == "completed"
                && qi.status != "completed"
            {
                // allow failed/cancelled to stay unless explicitly completed
            }
            qi.status = status.to_string();
            qi.finished_at = Some(now.clone());
            qi.updated_at = now;
            if last_error.is_some() {
                qi.last_error = last_error;
            }
            if let Err(e) = guard.queue.upsert(&qi) {
                tracing::warn!("mark_queue_terminal upsert failed item={item_id}: {e}");
            }
        }
        Ok(None) => tracing::warn!("mark_queue_terminal: item {item_id} not found"),
        Err(e) => tracing::warn!("mark_queue_terminal get failed item={item_id}: {e}"),
    }
}

/// Mark a loop terminal (`completed` / `failed` / `paused` / …).
pub fn mark_loop_status(repos: &Arc<SendRepos>, loop_id: &str, status: &str) {
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let Ok(guard) = repos.0.lock() else {
        tracing::warn!("mark_loop_status: lock poisoned (loop={loop_id})");
        return;
    };
    match guard.loops.get_by_id(loop_id) {
        Ok(Some(mut lp)) => {
            lp.status = status.to_string();
            lp.updated_at = now;
            if let Err(e) = guard.loops.upsert(&lp) {
                tracing::warn!("mark_loop_status upsert failed loop={loop_id}: {e}");
            }
        }
        Ok(None) => tracing::warn!("mark_loop_status: loop {loop_id} not found"),
        Err(e) => tracing::warn!("mark_loop_status get failed loop={loop_id}: {e}"),
    }
}

/// True if an active or terminal-completed queue item already exists for `dedupe_key`.
/// Used by discovery to avoid thrashing re-enqueues after success.
pub fn queue_dedupe_blocks_rediscovery(repos: &Arc<SendRepos>, dedupe_key: &str) -> bool {
    let Ok(guard) = repos.0.lock() else {
        return false;
    };
    if guard.queue.find_active_by_dedupe(dedupe_key).ok().flatten().is_some() {
        return true;
    }
    // Block re-discovery when a prior run already completed successfully.
    if let Ok(items) = guard.queue.list() {
        return items.iter().any(|q| q.dedupe_key == dedupe_key && q.status == "completed");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use looper_storage::migration::run_migrations;
    use looper_storage::record::QueueItemRecord;
    use looper_storage::repos::Repositories;
    use std::sync::Mutex;

    #[test]
    fn mark_queue_terminal_sets_completed() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        let repos = Repositories::new(conn);
        let now = "2026-01-01T00:00:00.000Z".to_string();
        let item = QueueItemRecord {
            id: "q1".into(),
            project_id: None,
            loop_id: None,
            r#type: "worker".into(),
            target_type: "issue".into(),
            target_id: "1".into(),
            repo: None,
            pr_number: None,
            dedupe_key: "d1".into(),
            priority: 1,
            status: "running".into(),
            available_at: now.clone(),
            attempts: 0,
            max_attempts: 3,
            claimed_by: Some("scheduler".into()),
            claimed_at: Some(now.clone()),
            started_at: Some(now.clone()),
            finished_at: None,
            lock_key: None,
            payload_json: None,
            last_error: None,
            last_error_kind: None,
            created_at: now.clone(),
            updated_at: now,
        };
        repos.queue.upsert(&item).unwrap();
        let wrapped = Arc::new(SendRepos(Mutex::new(repos)));
        mark_queue_terminal(&wrapped, "q1", "completed", None);
        let g = wrapped.0.lock().unwrap();
        let q = g.queue.get_by_id("q1").unwrap().unwrap();
        assert_eq!(q.status, "completed");
        assert!(q.finished_at.is_some());
    }
}
