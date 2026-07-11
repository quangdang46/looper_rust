use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use looper_storage::record::QueueItemRecord;

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Lightweight cancellation context passed through scheduler phases.
#[derive(Clone, Debug)]
pub struct Context {
    cancelled: Arc<AtomicBool>,
}

impl Context {
    pub fn new() -> Self {
        Self { cancelled: Arc::new(AtomicBool::new(false)) }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// QueuePriority constants
// ---------------------------------------------------------------------------

pub const QUEUE_PRIORITY_PLANNER: i64 = 1;
pub const QUEUE_PRIORITY_REVIEWER: i64 = 2;
pub const QUEUE_PRIORITY_FIXER: i64 = 2;
pub const QUEUE_PRIORITY_WORKER: i64 = 3;
pub const QUEUE_PRIORITY_SNAPSHOT: i64 = 4;

// ---------------------------------------------------------------------------
// Failure boundaries & classification
// ---------------------------------------------------------------------------

/// Categorisation of the external system that caused a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureBoundary {
    GitRemote,
    GitLocal,
    GitHubAPI,
    ModelProvider,
    AgentProcess,
    LocalWorktree,
    Storage,
    Config,
    Checkpoint,
    Policy,
    Unknown,
}

/// The kind of failure a queue item experienced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueueFailureKind {
    RetryableTransient,
    RetryableAfterResume,
    NonRetryable,
    ManualIntervention,
}

impl QueueFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            QueueFailureKind::RetryableTransient => "retryable_transient",
            QueueFailureKind::RetryableAfterResume => "retryable_after_resume",
            QueueFailureKind::NonRetryable => "non_retryable",
            QueueFailureKind::ManualIntervention => "manual_intervention",
        }
    }
}

/// Contextual info for failure classification.
#[derive(Debug, Clone)]
pub struct FailureClassificationContext {
    pub runner: String,
    pub step: String,
    pub boundary: FailureBoundary,
    pub side_effect_state: Option<String>,
}

// ---------------------------------------------------------------------------
// RunnerKind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunnerKind {
    Planner,
    Reviewer,
    Fixer,
    Worker,
    Coordinator,
    Snapshot,
}

impl RunnerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RunnerKind::Planner => "planner",
            RunnerKind::Reviewer => "reviewer",
            RunnerKind::Fixer => "fixer",
            RunnerKind::Worker => "worker",
            RunnerKind::Coordinator => "coordinator",
            RunnerKind::Snapshot => "snapshot",
        }
    }
}

// ---------------------------------------------------------------------------
// SchedulerConfig
// ---------------------------------------------------------------------------

/// Part of the global config that the scheduler reads.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub poll_interval: Duration,
    pub max_concurrent_runs: usize,
    pub retry_max_attempts: u32,
    pub retry_base_delay_ms: u64,
    pub slow_lane_warn_threshold: Duration,
    pub discovery_cache_ttl: Duration,
    pub dispatch_config: DispatchConfig,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(30),
            max_concurrent_runs: 1,
            retry_max_attempts: 3,
            retry_base_delay_ms: 1000,
            slow_lane_warn_threshold: Duration::from_secs(5),
            discovery_cache_ttl: Duration::from_secs(60),
            dispatch_config: DispatchConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DispatchMode {
    HumanGated,
    Autonomous,
}

impl Default for DispatchMode {
    fn default() -> Self {
        Self::Autonomous
    }
}

#[derive(Debug, Clone)]
pub struct DispatchAction {
    pub no_op: bool,
    pub trigger_labels: Vec<String>,
    pub assign_to: Option<String>,
    pub reaction_comment_id: Option<i64>,
    pub reaction_content: Option<String>,
    pub failure_comment_body: Option<String>,
}

impl DispatchAction {
    pub fn no_op() -> Self {
        Self {
            no_op: true,
            trigger_labels: vec![],
            assign_to: None,
            reaction_comment_id: None,
            reaction_content: None,
            failure_comment_body: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DispatchConfig {
    pub mode: DispatchMode,
    pub triaged_label: String,
    pub hold_label: Option<String>,
    pub autonomous_delay: chrono::Duration,
    pub allowed_users: Vec<String>,
    pub slash_commands: Vec<String>,
    pub assign_to: Option<String>,
    pub planner_trigger_labels: Vec<String>,
    pub worker_trigger_labels: Vec<String>,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            mode: DispatchMode::Autonomous,
            triaged_label: "looper:triaged".into(),
            hold_label: Some("looper:hold".into()),
            autonomous_delay: chrono::Duration::minutes(30),
            allowed_users: vec![],
            slash_commands: vec!["/plan".into(), "/implement".into()],
            assign_to: None,
            planner_trigger_labels: vec!["looper:plan".into()],
            worker_trigger_labels: vec!["looper:worker-ready".into()],
        }
    }
}

// ---------------------------------------------------------------------------
// ClaimPhase
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimPhase {
    PreDiscovery,
    PostPlannerDiscovery,
    PostCoordinatorDiscovery,
    PostReviewerDiscovery,
    PostFixerDiscovery,
    PostWorkerDiscovery,
    PostDiscovery,
    ClaimPump,
}

impl ClaimPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimPhase::PreDiscovery => "pre_discovery",
            ClaimPhase::PostPlannerDiscovery => "post_planner_discovery",
            ClaimPhase::PostCoordinatorDiscovery => "post_coordinator_discovery",
            ClaimPhase::PostReviewerDiscovery => "post_reviewer_discovery",
            ClaimPhase::PostFixerDiscovery => "post_fixer_discovery",
            ClaimPhase::PostWorkerDiscovery => "post_worker_discovery",
            ClaimPhase::PostDiscovery => "post_discovery",
            ClaimPhase::ClaimPump => "claim_pump",
        }
    }
}

// ---------------------------------------------------------------------------
// StaleRunReconcileMode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StaleRunReconcileMode {
    #[default]
    Startup,
    Live,
    Manual,
}

impl StaleRunReconcileMode {
    pub fn as_str(self) -> &'static str {
        match self {
            StaleRunReconcileMode::Startup => "startup",
            StaleRunReconcileMode::Live => "live",
            StaleRunReconcileMode::Manual => "manual",
        }
    }
}

// ---------------------------------------------------------------------------
// TickSummary / ClaimPassSummary
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct TickSummary {
    pub projects_processed: usize,
    pub total_claimed: usize,
    pub total_available: usize,
    pub discovery_errors: Vec<String>,
    pub duration: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct ClaimPassSummary {
    pub phase: Option<ClaimPhase>,
    pub claimed: usize,
    pub available: usize,
    pub duration: Duration,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// DiscoverySnapshot (placeholder — caches GitHub data per project per tick)
// ---------------------------------------------------------------------------

pub type DiscoverySnapshot = HashMap<String, String>;

// ---------------------------------------------------------------------------
// AsyncRunner trait
// ---------------------------------------------------------------------------

/// Fire-and-forget execution of asynchronous tasks.
pub trait AsyncRunner: Send + Sync {
    fn run(&self, f: Box<dyn FnOnce() + Send + 'static>);
}

/// Simple runner that spawns a `std::thread` per task.
pub struct ThreadRunner;

impl AsyncRunner for ThreadRunner {
    fn run(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        std::thread::spawn(f);
    }
}

// ---------------------------------------------------------------------------
// Handler traits — implemented by looper-runner (Phase 7)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PlannerDiscoveryInput {
    pub project_id: String,
    pub repo: String,
    pub snapshot: Option<Arc<DiscoverySnapshot>>,
}

#[derive(Debug, Clone, Default)]
pub struct PlannerDiscoveryResult {
    pub queue_items: Vec<QueueItemRecord>,
    pub created_loops: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PlannerProcessInput {
    pub item: QueueItemRecord,
}

#[derive(Debug, Clone, Default)]
pub struct PlannerProcessResult;

#[derive(Debug, Clone)]
pub struct CoordinatorDiscoveryInput {
    pub project_id: String,
    pub repo: String,
    pub snapshot: Option<Arc<DiscoverySnapshot>>,
}

#[derive(Debug, Clone, Default)]
pub struct CoordinatorDiscoveryResult {
    pub queue_items: Vec<QueueItemRecord>,
}

#[derive(Debug, Clone)]
pub struct ReviewerDiscoveryInput {
    pub project_id: String,
    pub repo: String,
    pub snapshot: Option<Arc<DiscoverySnapshot>>,
}

#[derive(Debug, Clone, Default)]
pub struct ReviewerDiscoveryResult {
    pub queue_items: Vec<QueueItemRecord>,
}

#[derive(Debug, Clone)]
pub struct ReviewerTargetedDiscoveryInput {
    pub project_id: String,
    pub repo: String,
    pub pr_number: i64,
}

#[derive(Debug, Clone)]
pub struct FixerDiscoveryInput {
    pub project_id: String,
    pub repo: String,
    pub snapshot: Option<Arc<DiscoverySnapshot>>,
}

#[derive(Debug, Clone, Default)]
pub struct FixerDiscoveryResult {
    pub queue_items: Vec<QueueItemRecord>,
}

#[derive(Debug, Clone)]
pub struct FixerBaseBranchDiscoveryInput {
    pub project_id: String,
    pub repo: String,
    pub base_branch: String,
}

#[derive(Debug, Clone)]
pub struct WorkerDiscoveryInput {
    pub project_id: String,
    pub repo: String,
    pub snapshot: Option<Arc<DiscoverySnapshot>>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkerIssueEntry {
    pub number: i64,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Default)]
pub struct WorkerDiscoveryResult {
    pub queue_items: Vec<QueueItemRecord>,
    pub issues: Vec<WorkerIssueEntry>,
}

#[derive(Debug, Clone)]
pub struct CapturePullRequestSnapshotInput {
    pub project_id: String,
    pub repo: String,
    pub pr_number: i64,
}

/// Trait for planner operations called by the scheduler.
pub trait PlannerScheduler: Send + Sync {
    fn discover_issues(&self, ctx: &Context, input: PlannerDiscoveryInput) -> PlannerDiscoveryResult;
    fn process_claimed_queue_item(&self, ctx: &Context, input: PlannerProcessInput) -> PlannerProcessResult;
}

/// Trait for coordinator operations called by the scheduler.
pub trait CoordinatorScheduler: Send + Sync {
    fn discover_issues(&self, ctx: &Context, input: CoordinatorDiscoveryInput) -> CoordinatorDiscoveryResult;
}

/// Trait for reviewer operations called by the scheduler.
pub trait ReviewerScheduler: Send + Sync {
    fn discover_pull_requests(&self, ctx: &Context, input: ReviewerDiscoveryInput) -> ReviewerDiscoveryResult;
    fn discover_pull_request(&self, ctx: &Context, input: ReviewerTargetedDiscoveryInput) -> ReviewerDiscoveryResult;
    fn process_claimed_queue_item(&self, ctx: &Context, item: &QueueItemRecord) -> Result<(), String>;
}

/// Trait for fixer operations called by the scheduler.
pub trait FixerScheduler: Send + Sync {
    fn discover_pull_requests(&self, ctx: &Context, input: FixerDiscoveryInput) -> FixerDiscoveryResult;
    fn discover_pull_request(&self, ctx: &Context, input: ReviewerTargetedDiscoveryInput) -> FixerDiscoveryResult;
    fn discover_pull_requests_for_base_branch_update(
        &self,
        ctx: &Context,
        input: FixerBaseBranchDiscoveryInput,
    ) -> FixerDiscoveryResult;
    fn process_claimed_queue_item(&self, ctx: &Context, item: &QueueItemRecord) -> Result<(), String>;
}

/// Trait for worker operations called by the scheduler.
pub trait WorkerScheduler: Send + Sync {
    fn discover_issues(&self, ctx: &Context, input: WorkerDiscoveryInput) -> WorkerDiscoveryResult;
    fn process_claimed_queue_item(&self, ctx: &Context, item: &QueueItemRecord) -> Result<(), String>;
}

/// Trait for snapshot operations called by the scheduler (run inline, not async).
pub trait SnapshotScheduler: Send + Sync {
    fn capture_pr_snapshot(&self, ctx: &Context, input: CapturePullRequestSnapshotInput) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// HandlerMap
// ---------------------------------------------------------------------------

pub struct HandlerMap {
    pub planner: Option<Arc<dyn PlannerScheduler>>,
    pub coordinator: Option<Arc<dyn CoordinatorScheduler>>,
    pub reviewer: Option<Arc<dyn ReviewerScheduler>>,
    pub fixer: Option<Arc<dyn FixerScheduler>>,
    pub worker: Option<Arc<dyn WorkerScheduler>>,
    pub snapshot: Option<Arc<dyn SnapshotScheduler>>,
}

impl HandlerMap {
    pub fn has_claim_handler(&self) -> bool {
        self.planner.is_some()
            || self.reviewer.is_some()
            || self.fixer.is_some()
            || self.worker.is_some()
            || self.snapshot.is_some()
    }
}

// ---------------------------------------------------------------------------
// StaleRunReconcileSummary
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct StaleRunReconcileSummary {
    pub mode: StaleRunReconcileMode,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub candidate_runs: u64,
    pub interrupted_runs: u64,
    pub loops_requeued: u64,
    pub queue_items_requeued: u64,
    pub queue_items_cancelled: u64,
    pub cleaned_executions: u64,
    pub skipped_uncertain_runs: u64,
    pub events_written: u64,
    pub run_ids: Vec<String>,
    pub loop_ids: Vec<String>,
    pub execution_ids: Vec<String>,
}
