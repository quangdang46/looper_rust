//! Middleware infrastructure for the Coordinator pipeline.
//!
//! Each middleware is a sync function that transforms `MiddlewareContext`.
//! The pipeline runs: quality gate → outcome recorder → patrol monitoring.

use std::sync::Arc;

use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::types::{Context, CoordinatorDiscoveryInput};
use looper_storage::record::{EventLogRecord, QueueItemRecord};

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Context passed through the middleware chain.
#[derive(Debug, Default, Clone)]
pub struct MiddlewareContext {
    pub queue_items: Vec<QueueItemRecord>,
    pub short_circuit: bool,
    pub data: std::collections::HashMap<String, String>,
}

impl MiddlewareContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: QueueItemRecord) -> &mut Self {
        self.queue_items.push(item);
        self
    }
}

// ---------------------------------------------------------------------------
// Public middleware functions (called directly by coordinator)
// ---------------------------------------------------------------------------

/// Quality gate: validates inputs, caps items, deduplicates against existing queue.
pub fn run_quality_gate(
    ctx: &mut MiddlewareContext,
    input: &CoordinatorDiscoveryInput,
    _scheduler_ctx: &Context,
    repos: &Arc<SendRepos>,
) -> Result<(), String> {
    // Input validation
    if input.project_id.is_empty() {
        return Err("project_id is empty".into());
    }
    if input.repo.is_empty() {
        return Err("repo is empty".into());
    }

    // Resource limits
    const MAX_ENQUEUE_PER_TICK: usize = 20;
    if ctx.queue_items.len() > MAX_ENQUEUE_PER_TICK {
        tracing::warn!("QualityGate: truncating {} items to max {MAX_ENQUEUE_PER_TICK}", ctx.queue_items.len());
        ctx.queue_items.truncate(MAX_ENQUEUE_PER_TICK);
    }

    // Duplicate detection
    let guard = repos.0.lock().map_err(|e| format!("repo lock: {e}"))?;

    let all_items: Vec<QueueItemRecord> = match guard.queue.list() {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!("QualityGate: list queue: {e}");
            vec![]
        }
    };

    let max_retries: i64 = 5;
    ctx.queue_items.retain(|item| {
        let already_pending = all_items
            .iter()
            .any(|q| q.dedupe_key == item.dedupe_key && (q.status == "queued" || q.status == "running"));
        let exhausted = all_items.iter().any(|q| q.dedupe_key == item.dedupe_key && q.attempts >= max_retries);

        let duplicate = already_pending;
        // Also check loop_id / pr_number
        let conflict = item.loop_id.as_deref().map_or(false, |lid| {
            all_items
                .iter()
                .any(|q| q.loop_id.as_deref() == Some(lid) && (q.status == "queued" || q.status == "running"))
        }) || item.pr_number.map_or(false, |pr| {
            all_items.iter().any(|q| q.pr_number == Some(pr) && (q.status == "queued" || q.status == "running"))
        });

        if duplicate || conflict {
            tracing::debug!("QualityGate: skipping duplicate/conflict {}", item.id);
        }
        if exhausted {
            tracing::warn!("QualityGate: exhausted after {max_retries} attempts: {}", item.id);
        }
        !(duplicate || conflict) && !exhausted
    });

    drop(guard);

    if ctx.queue_items.is_empty() {
        ctx.short_circuit = true;
    }

    Ok(())
}

/// Outcome recorder: persists each enqueued item as a pending outcome.
pub fn run_outcome_recorder(ctx: &MiddlewareContext, repos: &Arc<SendRepos>) -> Result<(), String> {
    let guard = repos.0.lock().map_err(|e| format!("repo lock: {e}"))?;

    for item in &ctx.queue_items {
        let outcome = looper_storage::record::OutcomeRecord {
            id: format!("outcome-{}", item.id),
            loop_id: item.loop_id.clone(),
            run_id: None,
            project_id: item.project_id.clone().unwrap_or_default(),
            repo: item.repo.clone(),
            loop_type: item.r#type.clone(),
            status: "queued".into(),
            duration_ms: None,
            exit_code: None,
            output_hash: None,
            error_message: None,
            error_kind: None,
            metadata_json: item.payload_json.clone(),
            created_at: item.created_at.clone(),
            updated_at: item.updated_at.clone(),
        };
        if let Err(e) = guard.outcomes.insert(&outcome) {
            tracing::warn!("OutcomeRecorder: insert failed: {e}");
        }
    }

    drop(guard);
    tracing::debug!("OutcomeRecorder: recorded {} outcomes", ctx.queue_items.len());
    Ok(())
}

/// Patrol: logs a patrol-active event for monitoring purposes.
pub fn run_patrol(ctx: &MiddlewareContext, repos: &Arc<SendRepos>) -> Result<(), String> {
    let guard = repos.0.lock().map_err(|e| format!("repo lock: {e}"))?;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string();
    for item in &ctx.queue_items {
        let payload = serde_json::json!({
            "action": "patrol_active",
            "dedupe_key": item.dedupe_key,
            "type": item.r#type,
        });
        let record = EventLogRecord {
            id: format!("patrol-{}-{}", item.id, chrono::Utc::now().timestamp_millis()),
            event_type: "patrol.active".into(),
            project_id: item.project_id.clone(),
            loop_id: item.loop_id.clone(),
            run_id: None,
            entity_type: Some("queue_item".into()),
            entity_id: Some(item.id.clone()),
            correlation_id: None,
            causation_id: None,
            actor_type: Some("system".into()),
            actor_id: Some("looperd".into()),
            actor_display_name: Some("looperd".into()),
            payload_json: payload.to_string(),
            created_at: now.clone(),
        };
        if let Err(e) = guard.events.append(&record) {
            tracing::warn!("Patrol: event append failed: {e}");
        }
    }

    drop(guard);
    tracing::debug!("Patrol: monitoring {} items", ctx.queue_items.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_middleware_context_push() {
        let mut ctx = MiddlewareContext::new();
        let item = QueueItemRecord {
            id: "test-1".into(),
            project_id: None,
            loop_id: None,
            r#type: "reviewer".into(),
            target_type: "pr".into(),
            target_id: "42".into(),
            dedupe_key: "test".into(),
            priority: 1,
            status: "queued".into(),
            available_at: "now".into(),
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
            repo: None,
            pr_number: None,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        ctx.push(item);
        assert_eq!(ctx.queue_items.len(), 1);
        assert!(!ctx.short_circuit);
    }
}
