use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::DomainError;
use crate::failure::FailureKind;

/// How to recover a loop after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumePolicy {
    /// Continue from the last recorded checkpoint.
    AdvanceFromCheckpoint,
    /// Require human approval before continuing.
    ManualIntervention,
    /// Re-run the step that failed.
    ReplayStep,
    /// Restart the loop from the `discover` step.
    RestartFromDiscover,
    /// Re-run the review step (reviewer loops).
    RerunReview,
    /// Retry from saved timeout context.
    RetryFromTimeoutContext,
}

impl ResumePolicy {
    /// Return the canonical string label.
    pub fn as_str(self) -> &'static str {
        use ResumePolicy::*;
        match self {
            AdvanceFromCheckpoint => "advance_from_checkpoint",
            ManualIntervention => "manual_intervention",
            ReplayStep => "replay_step",
            RestartFromDiscover => "restart_from_discover",
            RerunReview => "rerun_review",
            RetryFromTimeoutContext => "retry_from_timeout_context",
        }
    }

    /// Returns `true` if this policy blocks fully autonomous recovery.
    pub fn suppresses_autonomous_recovery(self) -> bool {
        matches!(self, ResumePolicy::ManualIntervention)
    }

    /// Returns `true` if the policy restarts from the discovery phase.
    pub fn should_restart_from_discover(self) -> bool {
        matches!(self, ResumePolicy::RestartFromDiscover)
    }

    /// Returns `true` if this policy places the loop on manual hold.
    pub fn is_manual_hold(self) -> bool {
        matches!(self, ResumePolicy::ManualIntervention | ResumePolicy::AdvanceFromCheckpoint)
    }
}

/// Default mapping from failure kind to resume policy.
impl From<FailureKind> for ResumePolicy {
    fn from(fk: FailureKind) -> Self {
        use FailureKind::*;
        match fk {
            RetryableTransient => ResumePolicy::RetryFromTimeoutContext,
            RetryableAfterResume => ResumePolicy::AdvanceFromCheckpoint,
            NonRetryable => ResumePolicy::ManualIntervention,
            ManualIntervention => ResumePolicy::ManualIntervention,
        }
    }
}

impl fmt::Display for ResumePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ResumePolicy {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use ResumePolicy::*;
        match s {
            "advance_from_checkpoint" => Ok(AdvanceFromCheckpoint),
            "manual_intervention" => Ok(ManualIntervention),
            "replay_step" => Ok(ReplayStep),
            "restart_from_discover" => Ok(RestartFromDiscover),
            "rerun_review" => Ok(RerunReview),
            "retry_from_timeout_context" => Ok(RetryFromTimeoutContext),
            _ => Err(DomainError::UnknownResumePolicy { value: s.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failure_kind_to_resume_policy() {
        assert_eq!(ResumePolicy::from(FailureKind::RetryableTransient), ResumePolicy::RetryFromTimeoutContext);
        assert_eq!(ResumePolicy::from(FailureKind::ManualIntervention), ResumePolicy::ManualIntervention);
        assert_eq!(ResumePolicy::from(FailureKind::NonRetryable), ResumePolicy::ManualIntervention);
        assert_eq!(ResumePolicy::from(FailureKind::RetryableAfterResume), ResumePolicy::AdvanceFromCheckpoint);
    }

    #[test]
    fn test_manual_intervention_suppresses_auto() {
        assert!(ResumePolicy::ManualIntervention.suppresses_autonomous_recovery());
        assert!(!ResumePolicy::RetryFromTimeoutContext.suppresses_autonomous_recovery());
    }

    #[test]
    fn test_roundtrip_serde() {
        for rp in [ResumePolicy::AdvanceFromCheckpoint, ResumePolicy::ManualIntervention, ResumePolicy::ReplayStep] {
            let json = serde_json::to_string(&rp).unwrap();
            let back: ResumePolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(rp, back);
        }
    }

    #[test]
    fn test_from_str() {
        assert_eq!("replay_step".parse(), Ok(ResumePolicy::ReplayStep));
        assert!("bogus".parse::<ResumePolicy>().is_err());
    }
}
