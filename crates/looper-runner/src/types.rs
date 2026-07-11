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
    /// Implementation PR needs automated fixer (criteria fail / CI red / CHANGES_REQUESTED).
    pub const NEEDS_FIX: &str = "looper:needs-fix";
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
// Spec-PR parsing helpers
// ---------------------------------------------------------------------------

/// Information parsed from a spec PR's body, labels, and review state.
///
/// Used by the Worker to discover planner-created spec context when
/// implementing an issue.
#[derive(Debug, Clone)]
pub struct SpecPRInfo {
    /// Which phase the spec PR is in (reviewing / ready / needs-human / unknown).
    pub phase: SpecPhase,
    /// Path to the spec file extracted from the PR body via `Spec:` regex.
    pub spec_path: String,
    /// True when any review carries CHANGES_REQUESTED state.
    pub has_changes_requested: bool,
    /// True when review is clean: no CHANGES_REQUESTED and phase is not NeedsHuman.
    pub review_clean: bool,
    /// PR number on GitHub (set after construction for convenience).
    pub pr_number: i64,
    /// Full PR body text for inline spec reference.
    pub body: String,
}

impl SpecPRInfo {
    /// Parse a spec PR's metadata from its body, labels, and review data.
    ///
    /// Extracts:
    /// - Spec path from `Spec:` line in PR body via case-insensitive regex
    ///   `/Spec:\s*(.+)$/im`
    /// - PR phase from labels using [`SpecPhase::from_labels`]
    /// - Review state from raw review JSON hashmaps
    pub fn parse(
        body: &str,
        labels: &[String],
        reviews: &[std::collections::HashMap<String, serde_json::Value>],
        pr_number: i64,
    ) -> Self {
        // Extract spec path via regex /Spec:\s*(.+)$/im
        let spec_re = regex::Regex::new(r"(?im)(?:^Spec:\s*(.+)$|^specPath:\s*(.+)$|^## Spec:\s*(.+)$)").unwrap();
        let spec_path = spec_re
            .captures(body)
            .and_then(|c| c.get(1).or_else(|| c.get(2)).or_else(|| c.get(3)))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();

        let phase = SpecPhase::from_labels(labels);

        let has_changes_requested =
            reviews.iter().any(|r| r.get("state").and_then(|v| v.as_str()) == Some("CHANGES_REQUESTED"));

        let review_clean = !has_changes_requested && phase != SpecPhase::NeedsHuman;

        Self { phase, spec_path, has_changes_requested, review_clean, pr_number, body: body.to_string() }
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
        DISCOVER_PR,
        CLAIM_PR,
        COLLECT_FIXES,
        PREPARE_WORKTREE,
        REPAIR,
        VALIDATE,
        PUSH,
        RECONCILE_COMMITS,
        RESOLVE_COMMENTS,
        RECHECK,
    ];
}

pub mod worker_steps {
    pub const PREPARE_WORK: &str = "prepare-work";
    pub const PREPARE_WORKTREE: &str = "prepare-worktree";
    pub const PLAN: &str = "plan";
    pub const EXECUTE: &str = "execute";
    pub const VALIDATE: &str = "validate";
    pub const OPEN_PR: &str = "open-pr";
    pub const ALL: &[&str] = &[PREPARE_WORK, PREPARE_WORKTREE, PLAN, EXECUTE, VALIDATE, OPEN_PR];
}

// ---------------------------------------------------------------------------
// Dispatch types (re-exported from looper-scheduler)
// ---------------------------------------------------------------------------

pub use looper_scheduler::types::{DispatchAction, DispatchConfig, DispatchMode};

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
        Self { transient_retries: capacity, max_indeterminate_duration_secs: 300, remaining_range: remaining }
    }

    pub fn remaining(&self) -> usize {
        self.remaining_range.end.saturating_sub(self.remaining_range.start)
    }
}

impl Default for RetryBudget {
    fn default() -> Self {
        Self { transient_retries: 5, max_indeterminate_duration_secs: 300, remaining_range: 0..0 }
    }
}

impl PartialEq for RetryBudget {
    fn eq(&self, other: &Self) -> bool {
        self.transient_retries == other.transient_retries
            && self.remaining_range.start == other.remaining_range.start
            && self.remaining_range.end == other.remaining_range.end
    }
}

// (DispatchConfig, DispatchMode, DispatchAction moved to looper-scheduler::types)
// ---------------------------------------------------------------------------
