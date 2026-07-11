use crate::loop_target::LoopTargetType;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::DomainError;

/// The 4 loop types — each defines a family of steps and compatible targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopType {
    Planner,
    Reviewer,
    Worker,
    Fixer,
}

impl LoopType {
    /// Return the canonical string label.
    pub fn as_str(self) -> &'static str {
        use LoopType::*;
        match self {
            Planner => "planner",
            Reviewer => "reviewer",
            Worker => "worker",
            Fixer => "fixer",
        }
    }

    /// Which target types are valid for this loop type.
    ///
    /// Planner may target a project (legacy discovery-wide) or a specific
    /// issue (issue → spec PR flow / admit-work). Reviewer/worker/fixer
    /// operate on pull requests or issues.
    pub fn allowed_target_types(self) -> &'static [LoopTargetType] {
        use LoopTargetType::*;
        use LoopType::*;
        match self {
            Planner => &[Project, Issue],
            Reviewer => &[PullRequest, Issue],
            Worker => &[PullRequest, Issue],
            Fixer => &[PullRequest, Issue],
        }
    }

    /// Validate that this loop type can be used with the given target type.
    pub fn supports_target_type(self, target_type: LoopTargetType) -> bool {
        self.allowed_target_types().contains(&target_type)
    }

    /// Return the steps that this loop type sequences through.
    pub fn steps(self) -> &'static [&'static str] {
        use LoopType::*;
        match self {
            Planner => &["discover", "assess", "plan", "present"],
            Reviewer => &["select", "review", "summarize", "decide"],
            Worker => &["select", "plan", "implement", "verify"],
            Fixer => &["analyze", "patch", "verify"],
        }
    }
}

impl fmt::Display for LoopType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LoopType {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use LoopType::*;
        match s {
            "planner" => Ok(Planner),
            "reviewer" => Ok(Reviewer),
            "worker" => Ok(Worker),
            "fixer" => Ok(Fixer),
            _ => Err(DomainError::UnknownLoopType { value: s.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LoopTargetType;

    #[test]
    fn test_planner_project_and_issue() {
        let lt = LoopType::Planner;
        assert!(lt.supports_target_type(LoopTargetType::Project));
        assert!(lt.supports_target_type(LoopTargetType::Issue));
        assert!(!lt.supports_target_type(LoopTargetType::PullRequest));
    }

    #[test]
    fn test_reviewer_works_with_pr_and_issue() {
        let lt = LoopType::Reviewer;
        assert!(!lt.supports_target_type(LoopTargetType::Project));
        assert!(lt.supports_target_type(LoopTargetType::PullRequest));
        assert!(lt.supports_target_type(LoopTargetType::Issue));
    }

    #[test]
    fn test_worker_and_fixer_same_as_reviewer() {
        for lt in [LoopType::Worker, LoopType::Fixer] {
            assert!(!lt.supports_target_type(LoopTargetType::Project));
            assert!(lt.supports_target_type(LoopTargetType::PullRequest));
            assert!(lt.supports_target_type(LoopTargetType::Issue));
        }
    }

    #[test]
    fn test_planner_steps() {
        assert_eq!(LoopType::Planner.steps(), &["discover", "assess", "plan", "present"]);
    }

    #[test]
    fn test_fixer_steps() {
        assert_eq!(LoopType::Fixer.steps(), &["analyze", "patch", "verify"]);
    }

    #[test]
    fn test_roundtrip_serde() {
        for lt in [LoopType::Planner, LoopType::Reviewer, LoopType::Worker, LoopType::Fixer] {
            let json = serde_json::to_string(&lt).unwrap();
            let back: LoopType = serde_json::from_str(&json).unwrap();
            assert_eq!(lt, back);
        }
    }

    #[test]
    fn test_from_str() {
        assert_eq!("planner".parse(), Ok(LoopType::Planner));
        assert!("unknown".parse::<LoopType>().is_err());
    }

    #[test]
    fn test_all_steps_are_non_empty() {
        for lt in [LoopType::Planner, LoopType::Reviewer, LoopType::Worker, LoopType::Fixer] {
            assert!(!lt.steps().is_empty(), "{:?} has no steps", lt);
        }
    }
}
