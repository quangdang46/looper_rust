use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::WebhookError;

// ---------------------------------------------------------------------------
// Core forwarder types
// ---------------------------------------------------------------------------

/// Uniquely identifies a piece of work for the forwarder.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct WorkKey {
    pub project_id: String,
    pub repo: String,
    pub object_type: String, // "pull_request" | "base_branch"
    pub number: i64,         // PR number (0 for base_branch)
    pub branch: String,      // branch name (empty for pull_request)
}

impl WorkKey {
    pub fn dedupe_key(&self, lane: Lane) -> String {
        format!("{}|{}|{}|{}|{}", self.project_id, self.object_type, self.number, self.branch, lane.as_str())
    }
}

/// Metadata about the triggering webhook event.
#[derive(Debug, Clone)]
pub struct WorkMetadata {
    pub event_type: String,
    pub action: String,
    pub delivery_id: String,
}

/// A work item enqueued for a set of lanes.
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub key: WorkKey,
    pub lanes: HashSet<Lane>,
    pub metadata: WorkMetadata,
    pub running: bool,
    pub enqueued: bool,
}

/// Lanes that work can be dispatched to.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum Lane {
    Reviewer,
    Fixer,
}

impl Lane {
    pub fn as_str(&self) -> &'static str {
        match self {
            Lane::Reviewer => "reviewer",
            Lane::Fixer => "fixer",
        }
    }
}

/// Incoming delivery request from a GitHub webhook.
#[derive(Debug, Clone)]
pub struct DeliveryRequest {
    pub delivery_id: String,
    pub event_type: String,
    pub payload: Vec<u8>,
}

/// Result returned to the API handler when Forward() is called.
#[derive(Debug, Clone)]
pub struct ForwardResult {
    pub status: String, // "accepted" | "ignored" | "duplicate"
    pub reason: String,
    pub work_items: i32,
}

/// Outcome of a single work execution (recorded for stats).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub at: String,
    pub project_id: String,
    pub repo: String,
    pub object_type: String,
    pub number: i64,
    pub lanes: Vec<String>,
    pub event_type: Option<String>,
    pub action: Option<String>,
    pub delivery_id: Option<String>,
    pub status: String, // "succeeded" | "failed"
    pub attempts: i32,
    pub error: Option<String>,
}

impl Outcome {
    /// Brief human-readable summary of this outcome for stats/logging.
    pub fn summary(&self) -> String {
        if self.status == "succeeded" {
            format!("ok  {} {} {} (#{})", self.repo, self.object_type, self.number, self.lanes.join(","))
        } else {
            format!(
                "FAIL {} {} {} (#{}) attempts={} err={:?}",
                self.repo,
                self.object_type,
                self.number,
                self.lanes.join(","),
                self.attempts,
                self.error,
            )
        }
    }
}

/// Aggregated stats for the forwarder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub deliveries_received: u64,
    pub deliveries_ignored: u64,
    pub work_items_created: u64,
    pub executions_succeeded: u64,
    pub executions_failed: u64,
    pub executions_retried: u64,
    pub in_flight: i32,
    pub queued: usize,
    pub recent_outcomes: VecDeque<Outcome>,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            deliveries_received: 0,
            deliveries_ignored: 0,
            work_items_created: 0,
            executions_succeeded: 0,
            executions_failed: 0,
            executions_retried: 0,
            in_flight: 0,
            queued: 0,
            recent_outcomes: VecDeque::with_capacity(64),
        }
    }
}

/// A record of an active or completed delivery (for dedup).
#[derive(Debug, Clone)]
pub struct DeliveryRecord {
    pub delivery_id: String,
    pub received_at: String,
    pub event_type: String,
    pub status: String,
}

// ---------------------------------------------------------------------------
// Forwarder inner state (protected by Mutex)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ForwarderInner {
    pub closed: bool,
    pub queue: Vec<WorkKey>,
    pub works: HashMap<String, WorkItem>,
    pub deliveries: HashMap<String, DeliveryRecord>,
    pub stats: Stats,
    pub recent_outcomes: VecDeque<Outcome>,
    pub current_in_flight: i32,
}

impl Default for ForwarderInner {
    fn default() -> Self {
        Self {
            closed: false,
            queue: Vec::new(),
            works: HashMap::new(),
            deliveries: HashMap::new(),
            stats: Stats::default(),
            recent_outcomes: VecDeque::with_capacity(64),
            current_in_flight: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Webhook envelope types (GitHub webhook payloads, deserialized)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PushEnvelope {
    pub r#ref: String,
    pub deleted: bool,
    pub repository: RepositoryRef,
}

#[derive(Debug, Deserialize)]
pub struct PullRequestEnvelope {
    pub action: String,
    pub repository: RepositoryRef,
    pub pull_request: PullRequestRef,
}

#[derive(Debug, Deserialize)]
pub struct IssueCommentEnvelope {
    pub action: String,
    pub repository: RepositoryRef,
    pub issue: IssueRef,
}

#[derive(Debug, Deserialize)]
pub struct CheckRunEnvelope {
    pub action: String,
    pub repository: RepositoryRef,
    pub check_run: CheckRunBody,
}

#[derive(Debug, Deserialize)]
pub struct RepositoryRef {
    pub full_name: String,
}

#[derive(Debug, Deserialize)]
pub struct PullRequestRef {
    pub number: i64,
}

#[derive(Debug, Deserialize)]
pub struct IssueRef {
    pub number: i64,
    pub pull_request: Option<PullRequestRef>,
}

#[derive(Debug, Deserialize)]
pub struct CheckRunBody {
    pub conclusion: String,
    pub pull_requests: Vec<PullRequestRef>,
    pub check_suite: CheckSuite,
}

#[derive(Debug, Deserialize)]
pub struct CheckSuite {
    pub pull_requests: Vec<PullRequestRef>,
}

// ---------------------------------------------------------------------------
// Targeted reviewer/fixer traits
// ---------------------------------------------------------------------------

/// Callback invoked by the forwarder to queue a review discovery.
#[async_trait::async_trait]
pub trait TargetedReviewer: Send + Sync {
    /// Create a queue item for a reviewer-targeted discovery.
    async fn enqueue_review(
        &self,
        project_id: &str,
        repo: &str,
        pr_number: i64,
        delivery_id: &str,
    ) -> Result<(), WebhookError>;
}

/// Callback invoked by the forwarder to queue a fixer discovery.
#[async_trait::async_trait]
pub trait TargetedFixer: Send + Sync {
    /// Create a queue item for a fixer-targeted discovery on a PR.
    async fn enqueue_fix_pr(
        &self,
        project_id: &str,
        repo: &str,
        pr_number: i64,
        delivery_id: &str,
    ) -> Result<(), WebhookError>;
    /// Create a queue item for a fixer-targeted discovery on a base branch.
    async fn enqueue_fix_branch(
        &self,
        project_id: &str,
        repo: &str,
        branch: &str,
        delivery_id: &str,
    ) -> Result<(), WebhookError>;
}

// ---------------------------------------------------------------------------
// Default implementations that create QueueItemRecords directly
// ---------------------------------------------------------------------------

use std::sync::Mutex;

use looper_storage::record::QueueItemRecord;
use looper_storage::repos::Repositories;

fn build_queue_item(
    project_id: &str,
    repo: &str,
    pr_number: Option<i64>,
    branch: Option<&str>,
    lane: &Lane,
    delivery_id: &str,
    now: &DateTime<Utc>,
) -> QueueItemRecord {
    let now_iso = now.to_rfc3339();
    let id = Uuid::new_v4().to_string();
    let (r#type, target_type, target_id, dedupe_key) = match lane {
        Lane::Reviewer => {
            let n = pr_number.unwrap_or(0);
            let dedupe = format!("{}|pull_request|{}|reviewer|{}", project_id, n, delivery_id);
            ("reviewer".into(), "pull_request".into(), n.to_string(), dedupe)
        }
        Lane::Fixer => match pr_number {
            Some(n) => {
                let dedupe = format!("{}|pull_request|{}|fixer|{}", project_id, n, delivery_id);
                ("fixer".into(), "pull_request".into(), n.to_string(), dedupe)
            }
            None => {
                let b = branch.unwrap_or("unknown");
                let dedupe = format!("{}|base_branch|0|fixer|{}", project_id, delivery_id);
                ("fixer".into(), "base_branch".into(), b.to_string(), dedupe)
            }
        },
    };

    QueueItemRecord {
        id,
        project_id: Some(project_id.to_string()),
        loop_id: None,
        r#type,
        target_type,
        target_id,
        repo: Some(repo.to_string()),
        pr_number,
        dedupe_key,
        priority: 0,
        status: "queued".to_string(),
        available_at: now_iso.clone(),
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
        created_at: now_iso.clone(),
        updated_at: now_iso,
    }
}

/// Wrapper around `Repositories` that provides `Send + Sync` safety.
/// This mirrors the pattern used in `looper-scheduler` for sharing rusqlite
/// connections across tokio tasks. The caller is responsible for serializing
/// access via the mutex.
pub struct SendRepos(pub Mutex<Repositories>);

// SAFETY: The rusqlite `Connection` is `!Sync` because it uses `RefCell`
// internally. We guarantee exclusive access through the `Mutex`, making
// this safe to send and share between threads.
unsafe impl Send for SendRepos {}
unsafe impl Sync for SendRepos {}

impl SendRepos {
    pub fn new(repos: Repositories) -> Self {
        Self(Mutex::new(repos))
    }
}

/// Default reviewer implementation that creates queue items via the repos.
pub struct DefaultTargetedReviewer {
    repos: Arc<SendRepos>,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl DefaultTargetedReviewer {
    pub fn new(repos: Arc<SendRepos>, now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>) -> Self {
        Self { repos, now }
    }
}

#[async_trait::async_trait]
impl TargetedReviewer for DefaultTargetedReviewer {
    async fn enqueue_review(
        &self,
        project_id: &str,
        repo: &str,
        pr_number: i64,
        delivery_id: &str,
    ) -> Result<(), WebhookError> {
        let now = (self.now)();
        let record = build_queue_item(project_id, repo, Some(pr_number), None, &Lane::Reviewer, delivery_id, &now);
        let repos = self.repos.0.lock().unwrap();
        repos.queue.upsert_active_by_dedupe_or_get_existing(&record)?;
        Ok(())
    }
}

/// Default fixer implementation that creates queue items via the repos.
pub struct DefaultTargetedFixer {
    repos: Arc<SendRepos>,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl DefaultTargetedFixer {
    pub fn new(repos: Arc<SendRepos>, now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>) -> Self {
        Self { repos, now }
    }
}

#[async_trait::async_trait]
impl TargetedFixer for DefaultTargetedFixer {
    async fn enqueue_fix_pr(
        &self,
        project_id: &str,
        repo: &str,
        pr_number: i64,
        delivery_id: &str,
    ) -> Result<(), WebhookError> {
        let now = (self.now)();
        let record = build_queue_item(project_id, repo, Some(pr_number), None, &Lane::Fixer, delivery_id, &now);
        let repos = self.repos.0.lock().unwrap();
        repos.queue.upsert_active_by_dedupe_or_get_existing(&record)?;
        Ok(())
    }

    async fn enqueue_fix_branch(
        &self,
        project_id: &str,
        repo: &str,
        branch: &str,
        delivery_id: &str,
    ) -> Result<(), WebhookError> {
        let now = (self.now)();
        let record = build_queue_item(project_id, repo, None, Some(branch), &Lane::Fixer, delivery_id, &now);
        let repos = self.repos.0.lock().unwrap();
        repos.queue.upsert_active_by_dedupe_or_get_existing(&record)?;
        Ok(())
    }
}
