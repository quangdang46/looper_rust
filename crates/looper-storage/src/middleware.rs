//! Scheduler-level middleware pipeline for post-discovery processing.
//!
//! Runs after all runners (planner, coordinator, reviewer, fixer, worker)
//! have enqueued items. Each middleware processes the full set of queue
//! items from the current tick.
//!
//! Pipeline: quality gate → outcome recording → patrol monitoring.

use crate::record::{EventLogRecord, OutcomeRecord, QueueItemRecord};

/// Run the full middleware pipeline on a batch of newly enqueued queue items.
///
/// Returns the filtered/sanitized set of items after quality gate passes.
pub fn run_post_discovery_middleware(items: &[QueueItemRecord], repos: &crate::Repositories) -> Vec<QueueItemRecord> {
    let mut items: Vec<QueueItemRecord> = items.to_vec();

    tracing::info!("mw: starting with {} items", items.len());

    // 1. Quality gate — deduplicate, cap resources
    items = run_quality_gate(items, repos);
    tracing::info!("mw: quality_gate -> {} items remain", items.len());

    if items.is_empty() {
        return items;
    }

    // 2. Outcome recorder — persist as pending outcomes
    match run_outcome_recorder(&items, repos) {
        Ok(_) => tracing::info!("mw: outcome_recorder done for {} items", items.len()),
        Err(e) => tracing::warn!("mw: outcome_recorder failed: {e}"),
    }

    // 3. Patrol — log monitoring event
    match run_patrol(&items, repos) {
        Ok(_) => tracing::info!("mw: patrol done for {} items", items.len()),
        Err(e) => tracing::warn!("mw: patrol failed: {e}"),
    }

    items
}

/// Quality gate: deduplicate against existing queue, cap items per tick.
fn run_quality_gate(items: Vec<QueueItemRecord>, repos: &crate::Repositories) -> Vec<QueueItemRecord> {
    const MAX_ENQUEUE_PER_TICK: usize = 20;
    let mut items = items;

    // Cap resources
    if items.len() > MAX_ENQUEUE_PER_TICK {
        tracing::warn!("quality_gate: truncating {} items to max {MAX_ENQUEUE_PER_TICK}", items.len());
        items.truncate(MAX_ENQUEUE_PER_TICK);
    }

    // Deduplicate against existing queue
    let existing = match repos.queue.list() {
        Ok(q) => q,
        Err(e) => {
            tracing::warn!("quality_gate: list queue: {e}");
            return items;
        }
    };

    let max_retries: i64 = 5;
    items.retain(|item| {
        let already_pending = existing.iter().any(|q| {
            q.id != item.id  // skip self-match
                && q.dedupe_key == item.dedupe_key
                && (q.status == "queued" || q.status == "running")
        });
        let exhausted =
            existing.iter().any(|q| q.id != item.id && q.dedupe_key == item.dedupe_key && q.attempts >= max_retries);
        // Also check loop_id / pr_number conflict
        let conflict = item.loop_id.as_deref().map_or(false, |lid| {
            existing.iter().any(|q| {
                q.id != item.id && q.loop_id.as_deref() == Some(lid) && (q.status == "queued" || q.status == "running")
            })
        }) || item.pr_number.map_or(false, |pr| {
            existing
                .iter()
                .any(|q| q.id != item.id && q.pr_number == Some(pr) && (q.status == "queued" || q.status == "running"))
        });

        if already_pending || conflict {
            tracing::debug!("quality_gate: skipping duplicate/conflict {}", item.id);
        }
        if exhausted {
            tracing::warn!("quality_gate: exhausted after {max_retries} attempts: {}", item.id);
        }
        !(already_pending || conflict) && !exhausted
    });

    items
}

/// Outcome recorder: persist each enqueued item as a pending outcome.
fn run_outcome_recorder(items: &[QueueItemRecord], repos: &crate::Repositories) -> Result<(), String> {
    for item in items {
        let outcome = OutcomeRecord {
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
        if let Err(e) = repos.outcomes.insert(&outcome) {
            tracing::warn!("outcome_recorder: insert failed: {e}");
        }
    }
    tracing::debug!("outcome_recorder: recorded {} outcomes", items.len());
    Ok(())
}

/// Patrol: log monitoring event for each enqueued item.
fn run_patrol(items: &[QueueItemRecord], repos: &crate::Repositories) -> Result<(), String> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string();
    for item in items {
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
        if let Err(e) = repos.events.append(&record) {
            tracing::warn!("patrol: event append failed: {e}");
        }
    }
    tracing::debug!("patrol: monitoring {} items", items.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_quality_gate_empty() {
        let result =
            run_quality_gate(vec![], &crate::Repositories::new(rusqlite::Connection::open_in_memory().unwrap()));
        assert!(result.is_empty());
    }
}
