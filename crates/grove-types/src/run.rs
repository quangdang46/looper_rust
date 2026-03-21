use crate::{
    BeadId, CheckpointId, HandoffRecord, RunId, Timestamp, errors::InvalidTransition,
    reaction::MutationStrategy,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// What an agent is DOING right now - used for proactive stuck detection.
///
/// This is distinct from RunStatus which tracks the run lifecycle.
/// Activity state enables autonomous response to stuck agents BEFORE timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentActivity {
    /// Agent is actively thinking/writing code.
    Active,
    /// Agent completed a unit of work, ready for next input.
    Ready,
    /// Agent has been idle for too long without progress.
    Idle,
    /// Agent encountered an error and cannot proceed.
    Blocked,
    /// Agent process has terminated.
    Exited,
    // NOTE: NO WaitingInput - Grove design avoids requiring user input mid-run.
}

impl AgentActivity {
    /// Map activity to autonomous action (NO human notification).
    #[must_use]
    pub fn autonomous_action(&self) -> AutonomousAction {
        match self {
            Self::Active => AutonomousAction::Continue,
            Self::Ready => AutonomousAction::CheckpointOrHandoff,
            Self::Idle => AutonomousAction::InjectRescuePrompt,
            Self::Blocked => AutonomousAction::RetryWithMutation,
            Self::Exited => AutonomousAction::RecoverOrFail,
        }
    }
}

/// Autonomous actions Grove can take in response to activity state.
///
/// All actions are fully autonomous - NO human notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutonomousAction {
    /// Continue current execution.
    Continue,
    /// Checkpoint progress and prepare handoff to next bead.
    CheckpointOrHandoff,
    /// Inject rescue prompt to unblock stuck agent.
    InjectRescuePrompt,
    /// Retry with mutation (narrow paths, different strategy).
    RetryWithMutation,
    /// Create recovery capsule and either restart or fail.
    RecoverOrFail,
}

/// Escalation tiers for progressive autonomous strategy.
///
/// Each tier represents a more aggressive strategy - NOT escalation to human.
/// The tier progresses when the previous strategy fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EscalationTier {
    /// First attempt - normal execution.
    #[default]
    FirstAttempt,
    /// Second attempt - inject rescue prompt.
    SecondAttempt,
    /// Third attempt - mutate strategy (narrow paths, different snippet).
    ThirdAttempt,
    /// Final attempt - drastic measures (switch model, re-bead).
    FinalAttempt,
    /// Give up - create recovery capsule and fail.
    GiveUp,
}

impl EscalationTier {
    /// Advance to the next escalation tier.
    #[must_use]
    pub fn escalate(&self) -> Self {
        match self {
            Self::FirstAttempt => Self::SecondAttempt,
            Self::SecondAttempt => Self::ThirdAttempt,
            Self::ThirdAttempt => Self::FinalAttempt,
            Self::FinalAttempt | Self::GiveUp => Self::GiveUp,
        }
    }

    /// Check if this is the final tier.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::GiveUp)
    }

    /// Get the tier number (0-indexed).
    #[must_use]
    pub fn tier_number(&self) -> u32 {
        match self {
            Self::FirstAttempt => 0,
            Self::SecondAttempt => 1,
            Self::ThirdAttempt => 2,
            Self::FinalAttempt => 3,
            Self::GiveUp => 4,
        }
    }
}

/// Policy for escalating through tiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationPolicy {
    /// Maximum number of tiers before giving up.
    pub max_tiers: u32,
    /// Whether to reset tier on successful progress.
    pub reset_on_progress: bool,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self {
            max_tiers: 4,
            reset_on_progress: true,
        }
    }
}

/// Why the coordinator stopped — persisted for post-mortem analysis.
///
/// This distinguishes user-triggered stops from crashes, timeouts, and
/// routine queue exhaustion so that `grove status` / `grove inspect`
/// can explain what happened after shutdown or restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoordinatorStopReason {
    /// User explicitly stopped via SIGINT/SIGTERM/Ctrl-C.
    UserStopped,
    /// Coordinator was interrupted by external signal during dispatch.
    Interrupted,
    /// No more dispatchable beads remain.
    QueueEmpty,
    /// Reached the maximum number of total dispatches.
    MaxRunsReached,
    /// Leader lease was contested/lost.
    LeaderContested,
    /// Exceeded the configured max poll cycles.
    MaxPollCycles,
    /// An unexpected error forced the coordinator to exit.
    InternalError,
}

impl CoordinatorStopReason {
    /// Human-readable label for status displays.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserStopped => "user_stopped",
            Self::Interrupted => "interrupted",
            Self::QueueEmpty => "queue_empty",
            Self::MaxRunsReached => "max_runs_reached",
            Self::LeaderContested => "leader_contested",
            Self::MaxPollCycles => "max_poll_cycles",
            Self::InternalError => "internal_error",
        }
    }

    /// Whether this reason represents a user-initiated stop.
    #[must_use]
    pub const fn is_user_initiated(self) -> bool {
        matches!(self, Self::UserStopped)
    }

    /// Whether this is a clean stop (vs. crash or error).
    #[must_use]
    pub const fn is_clean(self) -> bool {
        matches!(
            self,
            Self::UserStopped | Self::QueueEmpty | Self::MaxRunsReached | Self::MaxPollCycles
        )
    }
}

impl std::fmt::Display for CoordinatorStopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserStopped => write!(f, "user stopped (signal received)"),
            Self::Interrupted => write!(f, "interrupted during dispatch"),
            Self::QueueEmpty => write!(f, "no dispatchable beads remain"),
            Self::MaxRunsReached => write!(f, "reached max total runs"),
            Self::LeaderContested => write!(f, "leader lease contested"),
            Self::MaxPollCycles => write!(f, "exceeded max poll cycles"),
            Self::InternalError => write!(f, "internal error"),
        }
    }
}

impl EscalationTier {
    /// Get the default mutation strategy for this tier.
    #[must_use]
    pub fn default_mutation(&self) -> Option<MutationStrategy> {
        match self {
            Self::FirstAttempt => None,
            Self::SecondAttempt => None, // inject rescue prompt (not a mutation)
            Self::ThirdAttempt => Some(MutationStrategy::NarrowClaimedPaths),
            Self::FinalAttempt => Some(MutationStrategy::SwitchModel),
            Self::GiveUp => None, // create recovery capsule
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    Active,
    WaitingToRetry,
    Checkpointed,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureClass {
    Timeout,
    RateLimit,
    PermissionDenied,
    CircuitOpen,
    NoProgress,
    RepeatedError,
    ProtocolMalformed,
    ClaudeCrashed,
    BrMirrorFailed,
    Interrupted,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunRecord {
    pub id: crate::RunId,
    pub bead_id: BeadId,
    pub attempt_no: i32,
    pub status: RunStatus,
    pub failure_class: Option<FailureClass>,
    pub failure_detail: Option<String>,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub session_count: i32,
    pub checkpoint_count: i32,
    pub last_checkpoint_id: Option<CheckpointId>,
    /// Current activity state for proactive stuck detection.
    pub activity: Option<AgentActivity>,
    /// When the activity state was last updated.
    pub last_activity_at: Option<Timestamp>,
    /// Current escalation tier for progressive strategy.
    pub escalation_tier: EscalationTier,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub timeout_backoff_secs: u64,
    pub rate_limit_backoff_secs: u64,
    pub crash_backoff_secs: u64,
    pub no_progress_backoff_secs: u64,
    pub permission_denied_requires_manual_retry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCapsule {
    pub outcome: RecoveryCapsuleOutcome,
    pub summary: String,
    pub strongest_evidence: Vec<String>,
    pub likely_root_causes: Vec<String>,
    pub risky_paths: Vec<String>,
    pub do_not_repeat: Vec<String>,
    pub next_attempt_contract: Option<String>,
    pub retry_delta_summary: Option<String>,
    pub checkpoint_progress: Option<String>,
    pub checkpoint_next_step: Option<String>,
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryCapsuleOutcome {
    Failed,
    Interrupted,
    Checkpointed,
}

impl RecoveryCapsuleOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Checkpointed => "checkpointed",
        }
    }
}

impl RecoveryCapsule {
    #[must_use]
    pub fn compact_summary(&self) -> String {
        let mut parts = vec![self.summary.clone()];
        if let Some(next_step) = self.checkpoint_next_step.as_deref() {
            parts.push(format!("next: {next_step}"));
        }
        if let Some(delta) = self.retry_delta_summary.as_deref() {
            parts.push(delta.to_owned());
        }
        parts.join(" | ")
    }

    #[must_use]
    pub fn recommended_next_step(&self) -> Option<&str> {
        self.checkpoint_next_step
            .as_deref()
            .or(self.next_attempt_contract.as_deref())
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        outcome: RecoveryCapsuleOutcome,
        failure_class: Option<FailureClass>,
        failure_detail: Option<&str>,
        checkpoint_progress: Option<&str>,
        checkpoint_next_step: Option<&str>,
        next_attempt_contract: Option<&str>,
        retry_delta_summary: Option<&str>,
        artifacts: &[String],
    ) -> Option<Self> {
        if !matches!(outcome, RecoveryCapsuleOutcome::Checkpointed)
            && failure_class.is_none()
            && failure_detail.is_none()
            && checkpoint_progress.is_none()
            && checkpoint_next_step.is_none()
            && retry_delta_summary.is_none()
            && artifacts.is_empty()
        {
            return None;
        }

        let summary = match outcome {
            RecoveryCapsuleOutcome::Failed => match failure_class {
                Some(class) => format!("Run failed with {:?}", class),
                None => "Run failed before Grove captured a specific class".to_owned(),
            },
            RecoveryCapsuleOutcome::Interrupted => {
                "Run was interrupted after Grove had already persisted durable state".to_owned()
            }
            RecoveryCapsuleOutcome::Checkpointed => {
                "Run checkpointed with resumable progress captured for the next attempt".to_owned()
            }
        };

        let mut strongest_evidence = Vec::new();
        if let Some(detail) = failure_detail.filter(|detail| !detail.trim().is_empty()) {
            strongest_evidence.push(detail.trim().to_owned());
        }
        if let Some(progress) = checkpoint_progress.filter(|progress| !progress.trim().is_empty()) {
            strongest_evidence.push(format!("Checkpoint progress: {}", progress.trim()));
        }
        if let Some(next_step) =
            checkpoint_next_step.filter(|next_step| !next_step.trim().is_empty())
        {
            strongest_evidence.push(format!("Checkpoint next step: {}", next_step.trim()));
        }
        if let Some(delta) = retry_delta_summary.filter(|delta| !delta.trim().is_empty()) {
            strongest_evidence.push(format!("Retry delta: {}", delta.trim()));
        }
        if let Some(contract) = next_attempt_contract.filter(|contract| !contract.trim().is_empty())
        {
            strongest_evidence.push(format!("Next attempt contract: {}", contract.trim()));
        }

        let likely_root_causes = failure_class
            .map(|class| likely_root_causes_for_failure(class, failure_detail))
            .unwrap_or_else(|| likely_root_causes_for_outcome(outcome));
        let risky_paths = failure_class
            .map(risky_paths_for_failure)
            .unwrap_or_else(|| risky_paths_for_outcome(outcome));
        let do_not_repeat = failure_class
            .map(do_not_repeat_for_failure)
            .unwrap_or_else(|| do_not_repeat_for_outcome(outcome));

        let artifacts = artifacts
            .iter()
            .filter(|item| !item.trim().is_empty())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        Some(Self {
            outcome,
            summary,
            strongest_evidence,
            likely_root_causes,
            risky_paths,
            do_not_repeat,
            next_attempt_contract: next_attempt_contract.map(ToOwned::to_owned),
            retry_delta_summary: retry_delta_summary.map(ToOwned::to_owned),
            checkpoint_progress: checkpoint_progress.map(ToOwned::to_owned),
            checkpoint_next_step: checkpoint_next_step.map(ToOwned::to_owned),
            artifacts,
        })
    }
}

fn likely_root_causes_for_outcome(outcome: RecoveryCapsuleOutcome) -> Vec<String> {
    match outcome {
        RecoveryCapsuleOutcome::Failed => Vec::new(),
        RecoveryCapsuleOutcome::Interrupted => vec![
            "The run stopped externally even though Grove had already written recoverable state."
                .to_owned(),
        ],
        RecoveryCapsuleOutcome::Checkpointed => vec![
            "Grove intentionally rotated the session after capturing resumable progress."
                .to_owned(),
        ],
    }
}

fn risky_paths_for_outcome(outcome: RecoveryCapsuleOutcome) -> Vec<String> {
    match outcome {
        RecoveryCapsuleOutcome::Failed => Vec::new(),
        RecoveryCapsuleOutcome::Interrupted => vec![
            "Restarting from scratch instead of resuming from the durable checkpoint/handoff."
                .to_owned(),
        ],
        RecoveryCapsuleOutcome::Checkpointed => vec![
            "Repeating setup that the checkpoint already preserved before proving the next step."
                .to_owned(),
        ],
    }
}

fn do_not_repeat_for_outcome(outcome: RecoveryCapsuleOutcome) -> Vec<String> {
    match outcome {
        RecoveryCapsuleOutcome::Failed => Vec::new(),
        RecoveryCapsuleOutcome::Interrupted => {
            vec!["Do not discard the interrupted run's durable state when resuming.".to_owned()]
        }
        RecoveryCapsuleOutcome::Checkpointed => {
            vec!["Do not replay setup that the checkpoint already preserved.".to_owned()]
        }
    }
}

fn likely_root_causes_for_failure(
    failure_class: FailureClass,
    failure_detail: Option<&str>,
) -> Vec<String> {
    let mut causes = match failure_class {
        FailureClass::Timeout => vec![
            "The attempt sprawled instead of proving the smallest remaining step first.".to_owned(),
        ],
        FailureClass::RateLimit => vec![
            "The previous attempt used a tool or endpoint pattern that exceeded the rate window."
                .to_owned(),
        ],
        FailureClass::PermissionDenied => vec![
            "The previous attempt depended on an operation that was not allowed in the current session."
                .to_owned(),
        ],
        FailureClass::CircuitOpen | FailureClass::NoProgress => vec![
            "The session loop stopped producing new evidence and Grove opened recovery mode."
                .to_owned(),
        ],
        FailureClass::RepeatedError => vec![
            "The attempt kept returning to the same failing path instead of changing approach."
                .to_owned(),
        ],
        FailureClass::ProtocolMalformed => vec![
            "The session emitted invalid GROVE markers or malformed structured protocol output."
                .to_owned(),
        ],
        FailureClass::ClaudeCrashed => vec![
            "The Claude process terminated before the attempt could finish or checkpoint cleanly."
                .to_owned(),
        ],
        FailureClass::BrMirrorFailed => vec![
            "Implementation likely completed, but result mirroring back into br failed afterward."
                .to_owned(),
        ],
        FailureClass::Interrupted => vec![
            "A previously active run was discovered during recovery and marked interrupted.".to_owned(),
        ],
        FailureClass::Unknown => vec![
            "Grove did not capture a more specific terminal class for the failed run.".to_owned(),
        ],
    };

    if let Some(detail) = failure_detail.filter(|detail| !detail.trim().is_empty()) {
        causes.push(format!("Captured detail: {}", detail.trim()));
    }

    causes
}

fn risky_paths_for_failure(failure_class: FailureClass) -> Vec<String> {
    match failure_class {
        FailureClass::Timeout => {
            vec![
                "Broad replays that repeat setup before testing the highest-value remaining step."
                    .to_owned(),
            ]
        }
        FailureClass::RateLimit => {
            vec![
                "Immediate high-churn tool usage that re-enters the same rate-limit window."
                    .to_owned(),
            ]
        }
        FailureClass::PermissionDenied => {
            vec!["Retrying the blocked operation unchanged before exploring an already-allowed path.".to_owned()]
        }
        FailureClass::CircuitOpen | FailureClass::NoProgress => {
            vec![
                "Following the same debugging sequence that already failed to create progress."
                    .to_owned(),
            ]
        }
        FailureClass::RepeatedError => {
            vec![
                "Returning to the same failing code path without an explicit root-cause check."
                    .to_owned(),
            ]
        }
        FailureClass::ProtocolMalformed => {
            vec!["Ending the attempt without validating GROVE marker formatting.".to_owned()]
        }
        FailureClass::ClaudeCrashed => {
            vec!["Replaying already-completed setup instead of resuming from durable transcript and checkpoint state.".to_owned()]
        }
        FailureClass::BrMirrorFailed => {
            vec![
                "Redoing implementation work when only structured result reconstruction is needed."
                    .to_owned(),
            ]
        }
        FailureClass::Interrupted => {
            vec![
                "Restarting from scratch even though a partial run was already persisted."
                    .to_owned(),
            ]
        }
        FailureClass::Unknown => {
            vec![
                "Blindly replaying the last attempt without a concrete verification pivot."
                    .to_owned(),
            ]
        }
    }
}

fn do_not_repeat_for_failure(failure_class: FailureClass) -> Vec<String> {
    match failure_class {
        FailureClass::Timeout => {
            vec!["Do not replay the full attempt when a smaller unfinished step can prove progress first.".to_owned()]
        }
        FailureClass::RateLimit => {
            vec!["Do not hammer the same tools or endpoints back-to-back.".to_owned()]
        }
        FailureClass::PermissionDenied => {
            vec!["Do not repeat the blocked operation unchanged.".to_owned()]
        }
        FailureClass::CircuitOpen | FailureClass::NoProgress => {
            vec!["Do not repeat the same stalled inspection path verbatim.".to_owned()]
        }
        FailureClass::RepeatedError => {
            vec!["Do not repeat the same failing path before isolating root cause.".to_owned()]
        }
        FailureClass::ProtocolMalformed => {
            vec!["Do not finish the run without valid GROVE markers.".to_owned()]
        }
        FailureClass::ClaudeCrashed => {
            vec!["Do not redo already-completed setup just because the process crashed.".to_owned()]
        }
        FailureClass::BrMirrorFailed => {
            vec!["Do not re-implement completed code to recover a mirror failure.".to_owned()]
        }
        FailureClass::Interrupted => {
            vec![
                "Do not replay work already captured by the interrupted run's durable state."
                    .to_owned(),
            ]
        }
        FailureClass::Unknown => {
            vec!["Do not retry without changing the verification path.".to_owned()]
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderLeaseRecord {
    pub owner_label: String,
    pub run_id: Option<RunId>,
    pub acquired_at: Timestamp,
    pub heartbeat_at: Timestamp,
    pub expires_at: Timestamp,
    pub released_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MirrorStatus {
    Pending,
    InProgress,
    Succeeded,
    Failed,
}

impl MirrorStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MirrorOutboxRecord {
    pub id: String,
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub handoff: HandoffRecord,
    pub close_bead: bool,
    pub mirror_status: MirrorStatus,
    pub attempt_count: i32,
    pub last_attempt_at: Option<Timestamp>,
    pub next_retry_after: Option<Timestamp>,
    pub last_error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl TaskRunRecord {
    #[must_use]
    pub fn can_transition_to(&self, next: RunStatus) -> bool {
        use RunStatus::{Active, Checkpointed, Failed, Succeeded, WaitingToRetry};

        matches!(
            (self.status, next),
            (Active, Checkpointed)
                | (Active, WaitingToRetry)
                | (Active, Succeeded)
                | (Active, Failed)
                | (WaitingToRetry, Active)
                | (Checkpointed, Active)
        )
    }

    pub fn ensure_transition(self, next: RunStatus) -> Result<Self, InvalidTransition> {
        if self.can_transition_to(next) {
            Ok(Self {
                status: next,
                ..self
            })
        } else {
            Err(InvalidTransition::new(
                "run",
                format!("{:?}", self.status),
                format!("{:?}", next),
            ))
        }
    }
}
