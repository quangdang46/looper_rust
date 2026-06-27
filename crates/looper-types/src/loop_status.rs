use crate::error::DomainError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The 10 states in the loop lifecycle state machine.
///
/// # State classification
///
/// | Group          | States                                                     |
/// |----------------|------------------------------------------------------------|
/// | Terminal       | `Completed`, `Failed`, `Terminated`                        |
/// | Active running | `Running`, `Paused`, `Waiting`                             |
/// | Non-active     | `Idle`, `Queued`, `Stopped`, `Interrupted`                 |
///
/// # One-active-loop invariant
///
/// At most one loop may be in an **active-running** state per
/// `(loop_type, target_key)` pair.  Non-active and terminal states
/// do not conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStatus {
    Idle,
    Queued,
    Running,
    Paused,
    Waiting,
    Stopped,
    Terminated,
    Completed,
    Failed,
    Interrupted,
}

impl LoopStatus {
    /// Return `true` for states that are final — the execution phase is done.
    ///
    /// Note: `Failed` is terminal (execution stopped) but allows a `Queued` retry
    /// transition. `Completed` and `Terminated` are sink states with no outgoing
    /// transitions.
    pub fn is_terminal(self) -> bool {
        matches!(self, LoopStatus::Completed | LoopStatus::Failed | LoopStatus::Terminated)
    }

    /// Return `true` for states where the loop is actively executing or blocked.
    ///
    /// These states participate in the one-active-loop invariant.
    pub fn is_active_running(self) -> bool {
        matches!(self, LoopStatus::Running | LoopStatus::Paused | LoopStatus::Waiting)
    }

    /// Return `true` for states that represent a non-terminal, non-active condition.
    pub fn is_inactive(self) -> bool {
        !self.is_terminal() && !self.is_active_running()
    }

    /// Check whether a transition is allowed by the state machine.
    pub fn can_transition_to(self, to: LoopStatus) -> bool {
        use LoopStatus::*;
        matches!(
            (self, to),
            (Idle, Queued)
                | (Queued, Running | Stopped | Interrupted)
                | (Running, Paused | Waiting | Stopped | Terminated | Completed | Failed | Interrupted)
                | (Paused, Queued | Running | Stopped | Terminated | Interrupted)
                | (Waiting, Running | Stopped | Interrupted)
                | (Stopped, Queued)
                | (Failed, Queued)
                | (Interrupted, Queued)
        )
    }

    /// Attempt a transition; returns `Err` if the transition is invalid.
    pub fn transition(self, to: LoopStatus) -> Result<LoopStatus, DomainError> {
        if self.can_transition_to(to) {
            Ok(to)
        } else {
            Err(DomainError::InvalidLoopStatusTransition { from: self, to })
        }
    }

    /// Return the `serde` serialized name for this variant.
    pub fn as_str(self) -> &'static str {
        use LoopStatus::*;
        match self {
            Idle => "idle",
            Queued => "queued",
            Running => "running",
            Paused => "paused",
            Waiting => "waiting",
            Stopped => "stopped",
            Terminated => "terminated",
            Completed => "completed",
            Failed => "failed",
            Interrupted => "interrupted",
        }
    }
}

impl fmt::Display for LoopStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LoopStatus {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use LoopStatus::*;
        match s {
            "idle" => Ok(Idle),
            "queued" => Ok(Queued),
            "running" => Ok(Running),
            "paused" => Ok(Paused),
            "waiting" => Ok(Waiting),
            "stopped" => Ok(Stopped),
            "terminated" => Ok(Terminated),
            "completed" => Ok(Completed),
            "failed" => Ok(Failed),
            "interrupted" => Ok(Interrupted),
            _ => Err(DomainError::UnknownLoopStatus { value: s.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        assert!(LoopStatus::Idle.can_transition_to(LoopStatus::Queued));
        assert!(LoopStatus::Queued.can_transition_to(LoopStatus::Running));
        assert!(LoopStatus::Queued.can_transition_to(LoopStatus::Stopped));
        assert!(LoopStatus::Running.can_transition_to(LoopStatus::Completed));
        assert!(LoopStatus::Running.can_transition_to(LoopStatus::Failed));
        assert!(LoopStatus::Running.can_transition_to(LoopStatus::Paused));
        assert!(LoopStatus::Paused.can_transition_to(LoopStatus::Running));
        assert!(LoopStatus::Paused.can_transition_to(LoopStatus::Queued));
        assert!(LoopStatus::Waiting.can_transition_to(LoopStatus::Running));
        assert!(LoopStatus::Stopped.can_transition_to(LoopStatus::Queued));
        assert!(LoopStatus::Failed.can_transition_to(LoopStatus::Queued));
        assert!(LoopStatus::Interrupted.can_transition_to(LoopStatus::Queued));
    }

    #[test]
    fn test_sink_states_no_outgoing() {
        let sink = [LoopStatus::Completed, LoopStatus::Terminated];
        for from in sink {
            for to in [LoopStatus::Queued, LoopStatus::Running, LoopStatus::Paused] {
                assert!(!from.can_transition_to(to), "sink {:?} → {:?} should be invalid", from, to);
            }
        }
    }

    #[test]
    fn test_terminal_is_terminal() {
        for s in [LoopStatus::Completed, LoopStatus::Failed, LoopStatus::Terminated] {
            assert!(s.is_terminal());
        }
    }

    #[test]
    fn test_non_terminal_not_terminal() {
        for s in [LoopStatus::Idle, LoopStatus::Queued, LoopStatus::Running, LoopStatus::Paused] {
            assert!(!s.is_terminal());
        }
    }

    #[test]
    fn test_transition_returns_result() {
        assert_eq!(LoopStatus::Idle.transition(LoopStatus::Queued).unwrap(), LoopStatus::Queued);
        assert!(LoopStatus::Idle.transition(LoopStatus::Running).is_err());
    }

    #[test]
    fn test_roundtrip_serde() {
        for s in [LoopStatus::Idle, LoopStatus::Queued, LoopStatus::Running, LoopStatus::Completed] {
            let json = serde_json::to_string(&s).unwrap();
            let back: LoopStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn test_from_str() {
        assert_eq!("idle".parse(), Ok(LoopStatus::Idle));
        assert_eq!("running".parse(), Ok(LoopStatus::Running));
        assert!("invalid".parse::<LoopStatus>().is_err());
    }

    #[test]
    fn test_active_running_states() {
        for s in [LoopStatus::Running, LoopStatus::Paused, LoopStatus::Waiting] {
            assert!(s.is_active_running(), "{:?} should be active running", s);
        }
        assert!(!LoopStatus::Idle.is_active_running());
        assert!(!LoopStatus::Completed.is_active_running());
    }

    #[test]
    fn test_sink_states_no_transitions_at_all() {
        let sink = [LoopStatus::Completed, LoopStatus::Terminated];
        let all_states = [
            LoopStatus::Idle,
            LoopStatus::Queued,
            LoopStatus::Running,
            LoopStatus::Paused,
            LoopStatus::Waiting,
            LoopStatus::Stopped,
            LoopStatus::Terminated,
            LoopStatus::Completed,
            LoopStatus::Failed,
            LoopStatus::Interrupted,
        ];
        for from in sink {
            for to in all_states {
                assert!(!from.can_transition_to(to), "sink {:?} → {:?} should be invalid", from, to);
            }
        }
    }
}
