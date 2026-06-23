use std::pin::Pin;

use serde::{Deserialize, Serialize};

/// Alias for boxed future used by scheduler traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

// ---------------------------------------------------------------------------
// Re-exported types from downstream crates
// ---------------------------------------------------------------------------

/// Local copy of review decision enum so merge_watch can work without
/// coupling to looper_github's string-based representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    Dismissed,
}

impl ReviewDecision {
    pub fn from_github_string(s: &str) -> Option<Self> {
        match s {
            "APPROVED" => Some(Self::Approved),
            "CHANGES_REQUESTED" => Some(Self::ChangesRequested),
            "REVIEW_REQUIRED" => Some(Self::ReviewRequired),
            "DISMISSED" => Some(Self::Dismissed),
            _ => None,
        }
    }
}

/// Maximum consecutive retries for checks that report no change.
pub const NO_CHANGES_RETRY_LIMIT: usize = 10;

// ---------------------------------------------------------------------------
// Spec-PR phase labels
// ---------------------------------------------------------------------------

/// Labels used to track spec-PR lifecycle phases.
pub mod spec_labels {
    /// PR is under specification review (initial state after planner creates PR).
    pub const SPEC_REVIEWING: &str = "looper:spec-reviewing";
    /// Specification review passed — ready for implementation.
    pub const SPEC_READY: &str = "looper:spec-ready";
    /// Specification review found issues needing human intervention.
    pub const NEEDS_HUMAN: &str = "looper:needs-human";
}

/// Phase of a spec-PR determined by its current labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecPhase {
    SpecReviewing,
    SpecReady,
    NeedsHuman,
    Unknown,
}

impl SpecPhase {
    /// Determine the spec phase from a slice of label strings.
    ///
    /// Priority: `looper:needs-human` > `looper:spec-ready` > `looper:spec-reviewing`.
    /// If none match, returns `Unknown` (likely an implementation PR).
    pub fn from_labels(labels: &[String]) -> Self {
        if labels.iter().any(|l| l == spec_labels::NEEDS_HUMAN) {
            return Self::NeedsHuman;
        }
        if labels.iter().any(|l| l == spec_labels::SPEC_READY) {
            return Self::SpecReady;
        }
        if labels.iter().any(|l| l == spec_labels::SPEC_REVIEWING) {
            return Self::SpecReviewing;
        }
        Self::Unknown
    }
}

// ---------------------------------------------------------------------------
// Step constants — each runner's pipeline
// ---------------------------------------------------------------------------

pub mod planner_steps {
    pub const DISCOVER_ISSUES: &str = "discover-issues";
    pub const PREPARE_WORKTREE: &str = "prepare-worktree";
    pub const WRITE_SPEC: &str = "write-spec";
    pub const PUBLISH: &str = "publish";
    pub const NOTIFY: &str = "notify";
    pub const ALL: &[&str] = &[DISCOVER_ISSUES, PREPARE_WORKTREE, WRITE_SPEC, PUBLISH, NOTIFY];
}

pub mod reviewer_steps {
    pub const DISCOVER: &str = "discover";
    pub const FILTER: &str = "filter";
    pub const CLAIM: &str = "claim";
    pub const SNAPSHOT: &str = "snapshot";
    pub const PREPARE_WORKTREE: &str = "prepare-worktree";
    pub const REVIEW: &str = "review";
    pub const PUBLISH: &str = "publish";
    pub const ALL: &[&str] = &[DISCOVER, FILTER, CLAIM, SNAPSHOT, PREPARE_WORKTREE, REVIEW, PUBLISH];
}

pub mod fixer_steps {
    pub const DISCOVER_PR: &str = "discover-pr";
    pub const CLAIM_PR: &str = "claim-pr";
    pub const COLLECT_FIXES: &str = "collect-fixes";
    pub const PREPARE_WORKTREE: &str = "prepare-worktree";
    pub const REPAIR: &str = "repair";
    pub const VALIDATE: &str = "validate";
    pub const PUSH: &str = "push";
    pub const RECONCILE_COMMITS: &str = "reconcile-commits";
    pub const RESOLVE_COMMENTS: &str = "resolve-comments";
    pub const RECHECK: &str = "recheck";
    pub const ALL: &[&str] = &[
        DISCOVER_PR, CLAIM_PR, COLLECT_FIXES, PREPARE_WORKTREE, REPAIR, VALIDATE, PUSH,
        RECONCILE_COMMITS, RESOLVE_COMMENTS, RECHECK,
    ];
}

pub mod worker_steps {
    pub const PREPARE_WORK: &str = "prepare-work";
    pub const PREPARE_WORKTREE: &str = "prepare-worktree";
    pub const PLAN: &str = "plan";
    pub const EXECUTE: &str = "execute";
    pub const VALIDATE: &str = "validate";
    pub const OPEN_PR: &str = "open-pr";
    pub const ALL: &[&str] = &[
        PREPARE_WORK, PREPARE_WORKTREE, PLAN, EXECUTE, VALIDATE, OPEN_PR,
    ];
}

// ---------------------------------------------------------------------------
// Dispatch mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchMode {
    #[serde(rename = "human-gated")]
    HumanGated,
    #[serde(rename = "autonomous")]
    Autonomous,
}

/// Dispatch action returned by the decision engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

// ---------------------------------------------------------------------------
// MergeWatch types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PRSnapshot {
    pub repo: String,
    pub pr_number: i64,
    pub head_sha: String,
    pub merged: bool,
    pub open: bool,
    pub auto_merge_enabled: bool,
    pub auto_merge_owned_by_looper: bool,
    pub has_looper_label: bool,
    pub mergeable: Option<bool>,
    pub mergeable_state: String,
    pub required_checks: RequiredCheckSummary,
    pub temporary_error: Option<TemporaryError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RequiredCheckSummary {
    pub failed: Vec<String>,
    pub pending: Vec<String>,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporaryError {
    pub suggested_delay_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchActionKind {
    // ── Original merge-watch classifier ──
    Merged,
    StillPending,
    Indeterminate,
    Conflict,
    RedCI,
    BranchProtectionChanged,
    HumanDisabledAutoMerge,
    TransientError,

    // ── Extended for MergeWatch PR lifecycle ──
    MarkMerged,
    ClosePR,
    ReengageReview,
    MergeReady,
    RetryCheck,
    Stuck,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchAction {
    pub kind: WatchActionKind,

    // ── PR identification (used by Coordinator / MergeWatch) ──
    pub pr_number: i64,
    pub pr_title: String,
    pub snapshot: Option<PRSnapshot>,

    // ── Retry / deadline tracking ──
    pub first_unknown_at: Option<String>,
    pub deadline_exceeded: bool,
    pub retries_left: usize,
    pub suggested_delay_secs: u64,
    pub exhausted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorWatchMarker {
    pub pr_number: i64,
    pub head_sha: String,
    pub retries: usize,
    pub first_unknown_at: Option<String>,
    pub next_retry_at: Option<String>,
    pub review_decision: Option<ReviewDecision>,
}

/// A retry budget tracks how many transient/indeterminate attempts remain
/// before a watch action is classified as Stuck.
///
/// The second argument to `new()` is a `Range<usize>` representing how many
/// retries remain — `0..3` means 3 remaining, `3..3` means 0 (exhausted).
#[derive(Debug, Clone)]
pub struct RetryBudget {
    pub transient_retries: usize,
    pub max_indeterminate_duration_secs: u64,
    remaining_range: std::ops::Range<usize>,
}

impl RetryBudget {
    pub fn new(capacity: usize, remaining: std::ops::Range<usize>) -> Self {
        Self {
            transient_retries: capacity,
            max_indeterminate_duration_secs: 300,
            remaining_range: remaining,
        }
    }

    pub fn remaining(&self) -> usize {
        self.remaining_range.end.saturating_sub(self.remaining_range.start)
    }
}

impl Default for RetryBudget {
    fn default() -> Self {
        Self {
            transient_retries: 5,
            max_indeterminate_duration_secs: 300,
            remaining_range: 0..0,
        }
    }
}

impl PartialEq for RetryBudget {
    fn eq(&self, other: &Self) -> bool {
        self.transient_retries == other.transient_retries
            && self.remaining_range.start == other.remaining_range.start
            && self.remaining_range.end == other.remaining_range.end
    }
}

// ---------------------------------------------------------------------------
// Dispatch config
// ---------------------------------------------------------------------------

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
            mode: DispatchMode::HumanGated,
            triaged_label: "looper:triaged".into(),
            hold_label: Some("looper:hold".into()),
            autonomous_delay: chrono::Duration::minutes(30),
            allowed_users: vec![],
            slash_commands: vec!["/plan".into(), "/implement".into()],
            assign_to: None,
            planner_trigger_labels: vec!["looper:plan".into()],
            worker_trigger_labels: vec!["looper:implement".into()],
        }
    }
}
