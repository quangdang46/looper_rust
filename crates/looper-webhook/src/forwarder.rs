use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::error::{is_transient_error, WebhookError};
use crate::routing::{is_failing_conclusion, route_event, RoutingDecision};
use crate::types::{
    DeliveryRecord, DeliveryRequest, ForwardResult, ForwarderInner, Lane, Outcome, Stats, TargetedFixer,
    TargetedReviewer, WorkItem, WorkKey, WorkMetadata,
};

/// The maximum number of recent outcomes retained for stats reporting.
const RECENT_OUTCOME_LIMIT: usize = 64;

/// Default worker pool size.
const DEFAULT_MAX_CONCURRENT: usize = 4;

/// Default delivery TTL (1 hour).
const DEFAULT_DELIVERY_TTL_SECS: i64 = 3600;

/// Webhook event forwarder with a worker pool.
///
/// Receives incoming webhook delivery requests, routes them to the appropriate
/// lanes (reviewer / fixer), deduplicates, and dispatches them to the worker pool.
#[derive(Clone)]
pub struct WebhookForwarder {
    reviewer: Arc<dyn TargetedReviewer>,
    fixer: Arc<dyn TargetedFixer>,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,

    max_concurrent: usize,
    #[allow(dead_code)]
    delivery_ttl: Duration,
    retry_delay: Duration,

    inner: Arc<Mutex<ForwarderInner>>,
    notify: Arc<tokio::sync::Notify>,
}

impl WebhookForwarder {
    /// Create a new `WebhookForwarder`.
    pub fn new(reviewer: Arc<dyn TargetedReviewer>, fixer: Arc<dyn TargetedFixer>) -> Self {
        Self::with_options(reviewer, fixer, Options::default())
    }

    /// Create a new `WebhookForwarder` with custom options.
    pub fn with_options(reviewer: Arc<dyn TargetedReviewer>, fixer: Arc<dyn TargetedFixer>, opts: Options) -> Self {
        Self {
            reviewer,
            fixer,
            now: opts.now.unwrap_or_else(|| {
                let f: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync> = Arc::new(Utc::now);
                f
            }),
            max_concurrent: opts.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT),
            delivery_ttl: opts.delivery_ttl.unwrap_or(Duration::from_secs(DEFAULT_DELIVERY_TTL_SECS as u64)),
            retry_delay: opts.retry_delay.unwrap_or(Duration::from_secs(2)),
            inner: Arc::new(Mutex::new(ForwarderInner::default())),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Spawn the worker pool. Workers run until the forwarder is closed or
    /// all work is drained.
    pub async fn start(&self) {
        let mut handles = Vec::new();
        for i in 0..self.max_concurrent {
            let fwd = self.clone();
            let handle = tokio::spawn(async move {
                debug!("webhook worker {} started", i);
                fwd.worker_loop().await;
                debug!("webhook worker {} finished", i);
            });
            handles.push(handle);
        }
        // Note: callers should await or join the handles as needed.
        // The handles are intentionally dropped here — the tasks keep running.
        drop(handles);
    }

    /// Forward an incoming webhook delivery.
    ///
    /// Returns `ForwardResult` indicating whether the event was accepted,
    /// ignored, or deduplicated.
    pub async fn forward(&self, req: DeliveryRequest) -> ForwardResult {
        let mut inner = self.inner.lock().await;

        if inner.closed {
            return ForwardResult { status: "ignored".into(), reason: "forwarder is closed".into(), work_items: 0 };
        }

        inner.stats.deliveries_received += 1;

        // Deduplicate on delivery_id.
        if inner.deliveries.contains_key(&req.delivery_id) {
            return ForwardResult {
                status: "duplicate".into(),
                reason: "delivery already processed".into(),
                work_items: 0,
            };
        }

        inner.deliveries.insert(
            req.delivery_id.clone(),
            DeliveryRecord {
                delivery_id: req.delivery_id.clone(),
                received_at: (self.now)().to_rfc3339(),
                event_type: req.event_type.clone(),
                status: "processing".into(),
            },
        );

        // Parse and route.
        let payload_str = match String::from_utf8(req.payload.clone()) {
            Ok(s) => s,
            Err(_) => {
                inner.stats.deliveries_ignored += 1;
                return ForwardResult {
                    status: "ignored".into(),
                    reason: "payload is not valid UTF-8".into(),
                    work_items: 0,
                };
            }
        };

        let decision = self.route_payload(&req.event_type, &payload_str);

        if decision.is_ignored() {
            inner.stats.deliveries_ignored += 1;
            return ForwardResult {
                status: "ignored".into(),
                reason: "event type/action not routed".into(),
                work_items: 0,
            };
        }

        let work_keys = self.build_work_keys(&decision, &payload_str);
        let mut work_items_created = 0i32;

        for wk in &work_keys {
            if inner.works.contains_key(&wk.dedupe_key(Lane::Reviewer))
                || inner.works.contains_key(&wk.dedupe_key(Lane::Fixer))
            {
                continue; // already queued
            }

            let lanes = decision.lanes();
            let item = WorkItem {
                key: wk.clone(),
                lanes: lanes.clone(),
                metadata: WorkMetadata {
                    event_type: req.event_type.clone(),
                    action: extract_action(&decision, &payload_str),
                    delivery_id: req.delivery_id.clone(),
                },
                running: false,
                enqueued: false,
            };

            // Track per-lane in the works map.
            // The worker uses the lane-specific dedupe key.
            for lane in &lanes {
                let dk = wk.dedupe_key(lane.clone());
                inner.works.insert(dk.clone(), item.clone());
                inner.queue.push(wk.clone());
            }
            work_items_created += 1;
        }

        inner.stats.work_items_created += work_items_created as u64;
        inner.stats.queued = inner.queue.len();

        let count = work_items_created;

        // Notify workers that new work is available.
        self.notify.notify_waiters();

        ForwardResult { status: "accepted".into(), reason: "forwarded to worker pool".into(), work_items: count }
    }

    /// Get a snapshot of current stats.
    pub async fn stats(&self) -> Stats {
        let inner = self.inner.lock().await;
        inner.stats.clone()
    }

    /// Close the forwarder. Pending work is drained before workers exit.
    pub async fn close(&self) {
        let mut inner = self.inner.lock().await;
        inner.closed = true;
        self.notify.notify_waiters();
    }

    // ── Private helpers ──────────────────────────────────────────────

    fn route_payload(&self, event_type: &str, payload: &str) -> RoutingDecision {
        // For pull_request events, parse the action from JSON.
        match event_type {
            "pull_request" => {
                let action = extract_json_string(payload, "action");
                route_event(event_type, action.as_deref())
            }
            "check_run" => {
                let action = extract_json_string(payload, "action");
                match action.as_deref() {
                    Some("completed") => {
                        let conclusion = extract_json_string(payload, "check_run.conclusion");
                        match conclusion.as_deref() {
                            Some(c) if is_failing_conclusion(c) => RoutingDecision::CheckRun,
                            _ => RoutingDecision::Ignore,
                        }
                    }
                    _ => RoutingDecision::Ignore,
                }
            }
            "push" => {
                let deleted = extract_json_bool(payload, "deleted").unwrap_or(false);
                if deleted {
                    return RoutingDecision::Ignore;
                }
                route_event(event_type, None)
            }
            "issue_comment" => {
                // Only route issue_comment events on pull requests.
                let has_pr = extract_json_value(payload, "issue.pull_request").is_some();
                if has_pr {
                    // issue_comment on a PR is ignored per spec (issue_comment → ignore unconditional).
                    RoutingDecision::Ignore
                } else {
                    RoutingDecision::Ignore
                }
            }
            "pull_request_review" | "pull_request_review_comment" => route_event(event_type, None),
            _ => RoutingDecision::Ignore,
        }
    }

    fn build_work_keys(&self, decision: &RoutingDecision, payload: &str) -> Vec<WorkKey> {
        match decision {
            RoutingDecision::PullRequest(_) => {
                let repo = extract_json_string(payload, "repository.full_name").unwrap_or_default();
                let number = extract_json_i64(payload, "pull_request.number").unwrap_or(0);
                vec![WorkKey {
                    project_id: String::new(), // filled in worker
                    repo,
                    object_type: "pull_request".into(),
                    number,
                    branch: String::new(),
                }]
            }
            RoutingDecision::Push => {
                let repo = extract_json_string(payload, "repository.full_name").unwrap_or_default();
                let branch = extract_ref_branch(payload);
                vec![WorkKey { project_id: String::new(), repo, object_type: "base_branch".into(), number: 0, branch }]
            }
            RoutingDecision::CheckRun => {
                // Check runs may reference one or more PRs.
                let repo = extract_json_string(payload, "repository.full_name").unwrap_or_default();
                let pr_numbers = extract_pr_numbers_from_check_run(payload);
                if pr_numbers.is_empty() {
                    // Fallback: no PR association — try check_suite pull_requests.
                    let suite_prs = extract_json_array(payload, "check_run.check_suite.pull_requests");
                    if suite_prs.is_empty() {
                        vec![] // cannot route
                    } else {
                        suite_prs
                            .into_iter()
                            .filter_map(|v| v.get("number").and_then(|n| n.as_i64()))
                            .map(|n| WorkKey {
                                project_id: String::new(),
                                repo: repo.clone(),
                                object_type: "pull_request".into(),
                                number: n,
                                branch: String::new(),
                            })
                            .collect()
                    }
                } else {
                    pr_numbers
                        .into_iter()
                        .map(|n| WorkKey {
                            project_id: String::new(),
                            repo: repo.clone(),
                            object_type: "pull_request".into(),
                            number: n,
                            branch: String::new(),
                        })
                        .collect()
                }
            }
            RoutingDecision::Ignore => vec![],
        }
    }

    /// Worker loop: waits for work → executes with retry → records outcome.
    async fn worker_loop(&self) {
        loop {
            let (key, item) = {
                let mut inner = self.inner.lock().await;

                // Wait while no work and not closed.
                while inner.queue.is_empty() && !inner.closed {
                    drop(inner);
                    self.notify.notified().await;
                    inner = self.inner.lock().await;
                }

                if inner.closed && inner.queue.is_empty() {
                    break;
                }

                // Dequeue.
                let wk = inner.queue.remove(0);
                let dk_reviewer = wk.dedupe_key(Lane::Reviewer);
                let dk_fixer = wk.dedupe_key(Lane::Fixer);

                let item = inner.works.remove(&dk_reviewer).or_else(|| inner.works.remove(&dk_fixer));

                match item {
                    Some(item) => {
                        inner.current_in_flight += 1;
                        (wk, item)
                    }
                    None => continue,
                }
            };

            // Execute without holding the lock.
            let outcome = self.execute_with_retry(&key, &item).await;

            // Finish work (re-acquire lock).
            let mut inner = self.inner.lock().await;
            inner.current_in_flight -= 1;

            if outcome.status == "succeeded" {
                inner.stats.executions_succeeded += 1;
            } else {
                inner.stats.executions_failed += 1;
            }

            inner.stats.queued = inner.queue.len();
            inner.recent_outcomes.push_back(outcome.clone());
            while inner.recent_outcomes.len() > RECENT_OUTCOME_LIMIT {
                inner.recent_outcomes.pop_front();
            }
        }
    }

    /// Execute a work item with up to 2 retries.
    async fn execute_with_retry(&self, key: &WorkKey, item: &WorkItem) -> Outcome {
        let max_retries = 2;
        let mut outcome = Outcome {
            at: (self.now)().to_rfc3339(),
            project_id: key.project_id.clone(),
            repo: key.repo.clone(),
            object_type: key.object_type.clone(),
            number: key.number,
            lanes: item.lanes.iter().map(|l| l.as_str().to_string()).collect(),
            event_type: Some(item.metadata.event_type.clone()),
            action: Some(item.metadata.action.clone()),
            delivery_id: Some(item.metadata.delivery_id.clone()),
            attempts: 0,
            status: "failed".into(),
            error: None,
        };

        for attempt in 1..=max_retries {
            outcome.attempts = attempt;
            match self.execute_once(&item.key, item).await {
                Ok(()) => {
                    outcome.status = "succeeded".into();
                    return outcome;
                }
                Err(e) if attempt < max_retries && is_transient_error(&e) => {
                    info!(
                        "retrying webhook work for {} #{} (attempt {}/{})",
                        key.repo, key.number, attempt, max_retries
                    );
                    tokio::time::sleep(self.retry_delay).await;
                }
                Err(e) => {
                    outcome.status = "failed".into();
                    outcome.error = Some(e.to_string());
                    if attempt < max_retries {
                        // non-transient or exhausted — return final outcome.
                        return outcome;
                    }
                }
            }
        }

        outcome.status = "failed".into();
        outcome.error = Some("targeted discovery exhausted retries".into());
        outcome
    }

    /// Execute a single work item by calling the appropriate targeted handler.
    async fn execute_once(&self, key: &WorkKey, item: &WorkItem) -> Result<(), WebhookError> {
        // Use the project_id from the key (populated upstream by the route handler).
        if key.project_id.is_empty() {
            // Project ID resolution expected before this point;
            // fallback: look up from repo.
            warn!("execute_once called with empty project_id, repo={}", key.repo);
            return Err(WebhookError::NoProject(key.repo.clone()));
        }

        // Insert queue items for each lane.
        for lane in &item.lanes {
            match lane {
                Lane::Reviewer => {
                    self.reviewer
                        .enqueue_review(&key.project_id, &key.repo, key.number, &item.metadata.delivery_id)
                        .await?;
                }
                Lane::Fixer => {
                    if key.object_type == "pull_request" {
                        self.fixer
                            .enqueue_fix_pr(&key.project_id, &key.repo, key.number, &item.metadata.delivery_id)
                            .await?;
                    } else {
                        // base_branch
                        self.fixer
                            .enqueue_fix_branch(&key.project_id, &key.repo, &key.branch, &item.metadata.delivery_id)
                            .await?;
                    }
                }
            }
        }

        Ok(())
    }
}

/// Configuration options for the forwarder.
#[derive(Clone, Default)]
pub struct Options {
    pub now: Option<Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>>,
    pub max_concurrent: Option<usize>,
    pub delivery_ttl: Option<Duration>,
    pub retry_delay: Option<Duration>,
}

// ── JSON extraction helpers ──────────────────────────────────────────

/// Extract a string value from a JSON payload using a dot-separated path.
fn extract_json_string(payload: &str, path: &str) -> Option<String> {
    let val = extract_json_value(payload, path)?;
    val.as_str().map(|s| s.to_string())
}

/// Extract an i64 value from a JSON payload using a dot-separated path.
fn extract_json_i64(payload: &str, path: &str) -> Option<i64> {
    let val = extract_json_value(payload, path)?;
    val.as_i64()
}

/// Extract a bool value from a JSON payload using a dot-separated path.
fn extract_json_bool(payload: &str, path: &str) -> Option<bool> {
    let val = extract_json_value(payload, path)?;
    val.as_bool()
}

/// Extract a JSON value by a dot-separated path.
fn extract_json_value(payload: &str, path: &str) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = &v;
    for part in parts {
        current = current.get(part)?;
    }
    Some(current.clone())
}

/// Extract PR numbers from a check_run payload (check_run.pull_requests array).
fn extract_pr_numbers_from_check_run(payload: &str) -> Vec<i64> {
    let v: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let mut numbers = Vec::new();

    // Try check_run.pull_requests
    if let Some(prs) = v.pointer("/check_run/pull_requests").and_then(|v| v.as_array()) {
        for pr in prs {
            if let Some(n) = pr.get("number").and_then(|n| n.as_i64()) {
                numbers.push(n);
            }
        }
    }

    // Also try check_run.check_suite.pull_requests
    if numbers.is_empty() {
        if let Some(prs) = v.pointer("/check_run/check_suite/pull_requests").and_then(|v| v.as_array()) {
            for pr in prs {
                if let Some(n) = pr.get("number").and_then(|n| n.as_i64()) {
                    numbers.push(n);
                }
            }
        }
    }

    numbers
}

/// Extract an array of JSON values by path.
fn extract_json_array(payload: &str, path: &str) -> Vec<serde_json::Value> {
    let v: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = &v;
    for part in parts {
        current = match current.get(part) {
            Some(v) => v,
            None => return vec![],
        };
    }
    current.as_array().cloned().unwrap_or_default()
}

/// Extract the branch name from a push event `ref`.
fn extract_ref_branch(payload: &str) -> String {
    let r#ref = extract_json_string(payload, "ref").unwrap_or_default();
    // refs/heads/<branch>
    if let Some(branch) = r#ref.strip_prefix("refs/heads/") {
        branch.to_string()
    } else {
        r#ref
    }
}

/// Extract the action string from the routing decision.
fn extract_action(decision: &RoutingDecision, payload: &str) -> String {
    match decision {
        RoutingDecision::Push => "push".into(),
        RoutingDecision::CheckRun => "check_run".into(),
        RoutingDecision::PullRequest(_) => extract_json_string(payload, "action").unwrap_or_default(),
        RoutingDecision::Ignore => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_string() {
        let payload = r#"{"action": "opened", "repository": {"full_name": "owner/repo"}}"#;
        assert_eq!(extract_json_string(payload, "action"), Some("opened".into()));
        assert_eq!(extract_json_string(payload, "repository.full_name"), Some("owner/repo".into()));
        assert_eq!(extract_json_string(payload, "nonexistent"), None);
    }

    #[test]
    fn test_extract_json_i64() {
        let payload = r#"{"pull_request": {"number": 42}}"#;
        assert_eq!(extract_json_i64(payload, "pull_request.number"), Some(42));
    }

    #[test]
    fn test_extract_json_bool() {
        let payload = r#"{"deleted": false}"#;
        assert_eq!(extract_json_bool(payload, "deleted"), Some(false));
    }

    #[test]
    fn test_extract_ref_branch() {
        let payload = r#"{"ref": "refs/heads/main"}"#;
        assert_eq!(extract_ref_branch(payload), "main");
    }

    #[test]
    fn test_extract_ref_branch_no_prefix() {
        let payload = r#"{"ref": "main"}"#;
        assert_eq!(extract_ref_branch(payload), "main");
    }

    #[test]
    fn test_extract_pr_numbers_from_check_run() {
        let payload = r#"{
            "check_run": {
                "pull_requests": [{"number": 10}, {"number": 20}]
            }
        }"#;
        let nums = extract_pr_numbers_from_check_run(payload);
        assert_eq!(nums, vec![10, 20]);
    }

    #[test]
    fn test_extract_pr_numbers_from_check_run_fallback() {
        let payload = r#"{
            "check_run": {
                "pull_requests": [],
                "check_suite": {
                    "pull_requests": [{"number": 30}]
                }
            }
        }"#;
        let nums = extract_pr_numbers_from_check_run(payload);
        assert_eq!(nums, vec![30]);
    }
}
