//! Event-driven reactions system for autonomous recovery.
//!
//! Reactions are configurable rules that automatically respond to triggers
//! with autonomous actions - NO human notification.

use crate::{
    AgentActivity, EscalationTier, FailureClass, RecoveryCapsule, RecoveryCapsuleOutcome, RunId,
    RunStatus, Timestamp,
};
use serde::{Deserialize, Serialize};

/// Triggers that can invoke a reaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReactionTrigger {
    /// Circuit breaker opened due to repeated failures.
    CircuitOpen,
    /// No progress detected after multiple iterations.
    NoProgress { iterations: u32 },
    /// Agent has been idle for too long.
    AgentIdle { duration_secs: u64 },
    /// Context pressure is too high.
    ContextPressureHigh,
    /// Mirror operation failed.
    MirrorFailed,
    /// Retry budget exhausted.
    RetryBudgetExhausted,
}

/// Actions that reactions can take - all are fully autonomous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReactionAction {
    /// Inject a rescue prompt to unblock the agent.
    InjectRescue { prompt: String },
    /// Retry with a mutation strategy.
    RetryWithMutation { strategy: MutationStrategy },
    /// Force a checkpoint to preserve progress.
    ForceCheckpoint,
    /// Enqueue a mirror retry operation.
    EnqueueMirrorRetry,
    /// Schedule exponential backoff before next attempt.
    ScheduleBackoff { base_secs: u64 },
    /// Materialize a recovery capsule with an explicit outcome.
    CreateRecoveryCapsule { outcome: RecoveryCapsuleOutcome },
    /// Give up and create a terminal recovery capsule.
    GiveUp,
}

impl ReactionAction {
    /// Returns the recovery capsule outcome when the action materializes one directly.
    #[must_use]
    pub const fn recovery_capsule_outcome(&self) -> Option<RecoveryCapsuleOutcome> {
        match self {
            Self::CreateRecoveryCapsule { outcome } => Some(*outcome),
            Self::GiveUp => Some(RecoveryCapsuleOutcome::Failed),
            _ => None,
        }
    }
}

/// Strategies for mutating retry attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutationStrategy {
    /// Reduce the scope of claimed paths.
    NarrowClaimedPaths,
    /// Use a different archive snippet for context.
    DifferentArchiveSnippet,
    /// Try an alternative bead contract.
    AlternativeBeadContract,
    /// Reduce context window usage.
    ReduceContextWindow,
    /// Switch to a different model.
    SwitchModel,
}

/// Final disposition of a reaction invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReactionOutcome {
    /// The requested action was applied successfully.
    Applied,
    /// The reaction escalated to a fallback action.
    Escalated,
    /// The reaction was skipped because runtime state changed underneath it.
    Skipped,
    /// The reaction itself failed.
    Failed,
}

/// Snapshot of run state when a reaction fired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReactionContextSnapshot {
    /// Run lifecycle state when the reaction fired.
    pub run_status: Option<RunStatus>,
    /// Agent activity state when the reaction fired.
    pub activity: Option<AgentActivity>,
    /// Escalation tier when the reaction fired.
    pub escalation_tier: Option<EscalationTier>,
    /// Failure class driving the reaction, if any.
    pub failure_class: Option<FailureClass>,
    /// Failure detail captured before the reaction ran.
    pub failure_detail: Option<String>,
    /// Checkpoint progress available to the reaction.
    pub checkpoint_progress: Option<String>,
    /// Checkpoint next step available to the reaction.
    pub checkpoint_next_step: Option<String>,
    /// Retry delta summary already attached to the next prompt, if any.
    pub retry_delta_summary: Option<String>,
}

/// A rule that maps a trigger to an autonomous action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionRule {
    /// The trigger that invokes this reaction.
    pub trigger: ReactionTrigger,
    /// The action to take when triggered.
    pub action: ReactionAction,
    /// Whether this reaction is enabled.
    pub enabled: bool,
    /// Maximum number of times this reaction can be invoked.
    pub max_attempts: u32,
    /// Action to take if this reaction fails.
    pub escalate_to: Option<Box<ReactionAction>>,
}

impl Default for ReactionRule {
    fn default() -> Self {
        Self {
            trigger: ReactionTrigger::NoProgress { iterations: 3 },
            action: ReactionAction::InjectRescue {
                prompt: "You appear stuck. State one hypothesis before editing.".into(),
            },
            enabled: true,
            max_attempts: 3,
            escalate_to: None,
        }
    }
}

/// Record of a reaction invocation for post-mortem analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionRecord {
    /// Unique identifier for this reaction invocation.
    pub id: String,
    /// The trigger that caused this reaction.
    pub trigger: ReactionTrigger,
    /// The action that was taken.
    pub action: ReactionAction,
    /// Final disposition of the reaction invocation.
    pub outcome: ReactionOutcome,
    /// Escalated fallback action, if Grove had to pivot.
    pub escalated_to: Option<ReactionAction>,
    /// Recovery capsule emitted by this reaction, if any.
    pub recovery_capsule: Option<RecoveryCapsule>,
    /// Runtime snapshot captured when the reaction fired.
    pub context: Option<ReactionContextSnapshot>,
    /// When the reaction was invoked.
    pub invoked_at: Timestamp,
    /// Whether the reaction succeeded.
    pub success: bool,
    /// Error message if the reaction failed.
    pub error: Option<String>,
    /// The run this reaction was invoked for.
    pub run_id: Option<RunId>,
}

/// Default reaction rules for Grove.
pub fn default_reactions() -> Vec<ReactionRule> {
    vec![
        ReactionRule {
            trigger: ReactionTrigger::CircuitOpen,
            action: ReactionAction::InjectRescue {
                prompt: "Circuit breaker opened. State one hypothesis before editing.".into(),
            },
            enabled: true,
            max_attempts: 2,
            escalate_to: Some(Box::new(ReactionAction::CreateRecoveryCapsule {
                outcome: RecoveryCapsuleOutcome::Failed,
            })),
        },
        ReactionRule {
            trigger: ReactionTrigger::NoProgress { iterations: 3 },
            action: ReactionAction::InjectRescue {
                prompt: "No progress detected. State one hypothesis before editing.".into(),
            },
            enabled: true,
            max_attempts: 3,
            escalate_to: Some(Box::new(ReactionAction::RetryWithMutation {
                strategy: MutationStrategy::NarrowClaimedPaths,
            })),
        },
        ReactionRule {
            trigger: ReactionTrigger::AgentIdle { duration_secs: 300 },
            action: ReactionAction::InjectRescue {
                prompt: "Agent idle for 5 minutes. State what you're working on.".into(),
            },
            enabled: true,
            max_attempts: 2,
            escalate_to: Some(Box::new(ReactionAction::RetryWithMutation {
                strategy: MutationStrategy::ReduceContextWindow,
            })),
        },
        ReactionRule {
            trigger: ReactionTrigger::MirrorFailed,
            action: ReactionAction::EnqueueMirrorRetry,
            enabled: true,
            max_attempts: 5,
            escalate_to: Some(Box::new(ReactionAction::CreateRecoveryCapsule {
                outcome: RecoveryCapsuleOutcome::Failed,
            })),
        },
        ReactionRule {
            trigger: ReactionTrigger::ContextPressureHigh,
            action: ReactionAction::ForceCheckpoint,
            enabled: true,
            max_attempts: 1,
            escalate_to: Some(Box::new(ReactionAction::CreateRecoveryCapsule {
                outcome: RecoveryCapsuleOutcome::Checkpointed,
            })),
        },
        ReactionRule {
            trigger: ReactionTrigger::RetryBudgetExhausted,
            action: ReactionAction::CreateRecoveryCapsule {
                outcome: RecoveryCapsuleOutcome::Failed,
            },
            enabled: true,
            max_attempts: 1,
            escalate_to: None,
        },
    ]
}

#[cfg(test)]
mod tests;
