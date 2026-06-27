use std::sync::Arc;

use crate::error::{SchedulerError, SchedulerResult};
use crate::types::{
    CapturePullRequestSnapshotInput, Context, FixerScheduler, PlannerProcessInput, PlannerScheduler, ReviewerScheduler,
    SnapshotScheduler, WorkerScheduler,
};
use crate::Scheduler;
use looper_storage::record::QueueItemRecord;

pub fn claim_and_run(scheduler: &Scheduler, _ctx: &Context, available_slots: usize) -> Vec<QueueItemRecord> {
    let now_iso = format_javascript_iso_string((scheduler.now)().to_utc());
    let mut queue_items = Vec::with_capacity(available_slots);
    let repos = scheduler.repos();

    for _ in 0..available_slots {
        match repos.queue.claim_next_non_long_term_retry(&now_iso, "scheduler") {
            Ok(Some(item)) => queue_items.push(item),
            Ok(None) => break,
            Err(e) => {
                tracing::error!("claim_next_non_long_term_retry failed: {e}");
                break;
            }
        }
    }

    while queue_items.len() < available_slots {
        match repos.queue.claim_next_long_term_retry(&now_iso, "scheduler") {
            Ok(Some(item)) => queue_items.push(item),
            Ok(None) => break,
            Err(e) => {
                tracing::error!("claim_next_long_term_retry failed: {e}");
                break;
            }
        }
    }

    drop(repos);

    if !queue_items.is_empty() {
        dispatch_queue_items(scheduler, &queue_items);
    }

    queue_items
}

fn dispatch_queue_items(scheduler: &Scheduler, items: &[QueueItemRecord]) {
    for item in items {
        let processor = match resolve_processor(scheduler, item) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("failed to resolve processor for item {} (type={}): {e}", item.id, item.r#type);
                continue;
            }
        };

        let item_id = item.id.clone();
        let item_type = item.r#type.clone();

        let f: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
            if let Err(e) = processor.process() {
                tracing::error!("queue item processing failed: type={} id={} error={e}", item_type, item_id);
            }
        });

        scheduler.async_runner.run(f);
    }
}

fn resolve_processor(scheduler: &Scheduler, item: &QueueItemRecord) -> SchedulerResult<Box<dyn QueueItemProcessor>> {
    match item.r#type.as_str() {
        "planner" => {
            let p = scheduler
                .handlers
                .planner
                .as_ref()
                .ok_or_else(|| SchedulerError::HandlerNotConfigured("planner".into()))?;
            Ok(Box::new(PlannerItemProcessor { handler: Arc::clone(p), item: item.clone() }))
        }
        "reviewer" => {
            let r = scheduler
                .handlers
                .reviewer
                .as_ref()
                .ok_or_else(|| SchedulerError::HandlerNotConfigured("reviewer".into()))?;
            Ok(Box::new(ReviewerItemProcessor { handler: Arc::clone(r), item: item.clone() }))
        }
        "fixer" => {
            let f = scheduler
                .handlers
                .fixer
                .as_ref()
                .ok_or_else(|| SchedulerError::HandlerNotConfigured("fixer".into()))?;
            Ok(Box::new(FixerItemProcessor { handler: Arc::clone(f), item: item.clone() }))
        }
        "worker" => {
            let w = scheduler
                .handlers
                .worker
                .as_ref()
                .ok_or_else(|| SchedulerError::HandlerNotConfigured("worker".into()))?;
            Ok(Box::new(WorkerItemProcessor { handler: Arc::clone(w), item: item.clone() }))
        }
        "snapshot" => {
            let s = scheduler
                .handlers
                .snapshot
                .as_ref()
                .ok_or_else(|| SchedulerError::HandlerNotConfigured("snapshot".into()))?;
            Ok(Box::new(SnapshotItemProcessor { handler: Arc::clone(s), item: item.clone() }))
        }
        other => Err(SchedulerError::UnresolvableProcessor(other.to_string())),
    }
}

pub trait QueueItemProcessor: Send {
    fn process(&self) -> Result<(), String>;
}

struct PlannerItemProcessor {
    handler: Arc<dyn PlannerScheduler>,
    item: QueueItemRecord,
}

impl QueueItemProcessor for PlannerItemProcessor {
    fn process(&self) -> Result<(), String> {
        let ctx = Context::new();
        let input = PlannerProcessInput { item: self.item.clone() };
        self.handler.process_claimed_queue_item(&ctx, input);
        Ok(())
    }
}

struct ReviewerItemProcessor {
    handler: Arc<dyn ReviewerScheduler>,
    item: QueueItemRecord,
}

impl QueueItemProcessor for ReviewerItemProcessor {
    fn process(&self) -> Result<(), String> {
        let ctx = Context::new();
        self.handler.process_claimed_queue_item(&ctx, &self.item)
    }
}

struct FixerItemProcessor {
    handler: Arc<dyn FixerScheduler>,
    item: QueueItemRecord,
}

impl QueueItemProcessor for FixerItemProcessor {
    fn process(&self) -> Result<(), String> {
        let ctx = Context::new();
        self.handler.process_claimed_queue_item(&ctx, &self.item)
    }
}

struct WorkerItemProcessor {
    handler: Arc<dyn WorkerScheduler>,
    item: QueueItemRecord,
}

impl QueueItemProcessor for WorkerItemProcessor {
    fn process(&self) -> Result<(), String> {
        let ctx = Context::new();
        self.handler.process_claimed_queue_item(&ctx, &self.item)
    }
}

struct SnapshotItemProcessor {
    handler: Arc<dyn SnapshotScheduler>,
    item: QueueItemRecord,
}

impl QueueItemProcessor for SnapshotItemProcessor {
    fn process(&self) -> Result<(), String> {
        let ctx = Context::new();
        let input = CapturePullRequestSnapshotInput {
            project_id: self.item.project_id.clone().unwrap_or_default(),
            repo: self.item.repo.clone().unwrap_or_default(),
            pr_number: self.item.pr_number.unwrap_or(0),
        };
        self.handler.capture_pr_snapshot(&ctx, input)
    }
}

fn format_javascript_iso_string(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S.000Z").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_javascript_iso_string() {
        use chrono::TimeZone;
        let dt = chrono::Utc.with_ymd_and_hms(2026, 6, 22, 10, 0, 0).unwrap();
        let formatted = format_javascript_iso_string(dt);
        assert_eq!(formatted, "2026-06-22T10:00:00.000Z");
    }
}
