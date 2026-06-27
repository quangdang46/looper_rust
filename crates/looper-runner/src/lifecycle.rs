//! Lifecycle policy: agent-vs-human action tracking.
//!
//! Ported from Go `legacy/internal/lifecycle/lifecycle.go` (474 LOC).
//!
//! Tracks who is responsible for each action in a run: was a commit made
//! by an AI agent or by a human fallback? This is crucial for audit and
//! determining how much autonomy the system actually has.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default lifecycle policy name.
pub const POLICY_AGENT_MANAGED_WITH_FALLBACK: &str = "agent_managed_with_fallback";
/// Current policy version.
pub const POLICY_VERSION: i32 = 1;

/// Action performed by the agent (AI).
pub const ACTION_SOURCE_AGENT: &str = "agent";
/// Action performed by a human fallback.
pub const ACTION_SOURCE_FALLBACK: &str = "fallback";
/// Action not yet performed.
pub const ACTION_SOURCE_NONE: &str = "none";

/// Branch was planned (created from a spec).
pub const BRANCH_PROVENANCE_PLANNED: &str = "planned";
/// Branch was created/migrated by the agent.
pub const BRANCH_PROVENANCE_AGENT_MIGRATED: &str = "agent_migrated";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A lifecycle policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    pub name: String,
    pub version: i32,
    #[serde(default)]
    pub expect_commit: bool,
    #[serde(default)]
    pub expect_push: bool,
    #[serde(default)]
    pub expect_pr: bool,
    #[serde(default)]
    pub allow_fallback: bool,
    #[serde(default)]
    pub eligible_runners: Vec<String>,
}

/// Which agent or fallback performed each action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct Actions {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub push: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pr: String,
}

impl Actions {}

/// Complete lifecycle state for a run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct State {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy: String,
    #[serde(default)]
    pub policy_version: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_branch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub planned_branch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub planned_base_branch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_branch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_base_branch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub active_branch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub active_base_branch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch_provenance: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commit_shas: Vec<String>,
    #[serde(default)]
    pub pushed: bool,
    #[serde(default)]
    pub pr_number: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pr_url: String,
    #[serde(default)]
    pub pr_adopted: bool,
    #[serde(default)]
    pub actions: Actions,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reconciled_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reconciled_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_error: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_ingested_at: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            policy: POLICY_AGENT_MANAGED_WITH_FALLBACK.into(),
            policy_version: POLICY_VERSION,
            branch: String::new(),
            base_branch: String::new(),
            planned_branch: String::new(),
            planned_base_branch: String::new(),
            agent_branch: String::new(),
            agent_base_branch: String::new(),
            active_branch: String::new(),
            active_base_branch: String::new(),
            branch_provenance: String::new(),
            commit_shas: vec![],
            pushed: false,
            pr_number: 0,
            pr_url: String::new(),
            pr_adopted: false,
            actions: Actions::default(),
            reconciled_at: String::new(),
            reconciled_by: String::new(),
            last_error: String::new(),
            agent_ingested_at: String::new(),
        }
    }
}

/// Custom deserialization that accepts both snake_case and camelCase JSON keys.
impl State {
    /// Deserialize from a JSON value, accepting both snake_case and camelCase keys.
    pub fn from_json_value(value: &serde_json::Value) -> Result<Self, String> {
        let map = match value {
            serde_json::Value::Object(m) => m,
            _ => return Err("expected JSON object".into()),
        };
        let mut s: State =
            serde_json::from_value(serde_json::Value::Object(map.clone())).map_err(|e| format!("deserialize: {e}"))?;

        // Check for camelCase keys that didn't match snake_case fields
        let camel_to_snake = |k: &str| -> String {
            let mut out = String::new();
            for (i, c) in k.chars().enumerate() {
                if c.is_uppercase() && i > 0 {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
            }
            out
        };

        for (key, val) in map {
            let snake = camel_to_snake(key);
            match snake.as_str() {
                "policy_version" => {
                    if s.policy_version == 0 {
                        s.policy_version = val.as_i64().unwrap_or(0) as i32;
                    }
                }
                "base_branch" => {
                    if s.base_branch.is_empty() {
                        s.base_branch = val.as_str().unwrap_or("").to_string();
                    }
                }
                "planned_branch" => {
                    if s.planned_branch.is_empty() {
                        s.planned_branch = val.as_str().unwrap_or("").to_string();
                    }
                }
                "planned_base_branch" => {
                    if s.planned_base_branch.is_empty() {
                        s.planned_base_branch = val.as_str().unwrap_or("").to_string();
                    }
                }
                "agent_branch" => {
                    if s.agent_branch.is_empty() {
                        s.agent_branch = val.as_str().unwrap_or("").to_string();
                    }
                }
                "agent_base_branch" => {
                    if s.agent_base_branch.is_empty() {
                        s.agent_base_branch = val.as_str().unwrap_or("").to_string();
                    }
                }
                "active_branch" => {
                    if s.active_branch.is_empty() {
                        s.active_branch = val.as_str().unwrap_or("").to_string();
                    }
                }
                "active_base_branch" => {
                    if s.active_base_branch.is_empty() {
                        s.active_base_branch = val.as_str().unwrap_or("").to_string();
                    }
                }
                "branch_provenance" => {
                    if s.branch_provenance.is_empty() {
                        s.branch_provenance = val.as_str().unwrap_or("").to_string();
                    }
                }
                "commit_shas" => {
                    if s.commit_shas.is_empty() {
                        s.commit_shas = val
                            .as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default();
                    }
                }
                "pr_number" => {
                    if s.pr_number == 0 {
                        s.pr_number = val.as_i64().unwrap_or(0);
                    }
                }
                "pr_url" => {
                    if s.pr_url.is_empty() {
                        s.pr_url = val.as_str().unwrap_or("").to_string();
                    }
                }
                "pr_adopted" => {
                    if !s.pr_adopted {
                        s.pr_adopted = val.as_bool().unwrap_or(false);
                    }
                }
                "reconciled_at" => {
                    if s.reconciled_at.is_empty() {
                        s.reconciled_at = val.as_str().unwrap_or("").to_string();
                    }
                }
                "reconciled_by" => {
                    if s.reconciled_by.is_empty() {
                        s.reconciled_by = val.as_str().unwrap_or("").to_string();
                    }
                }
                "last_error" => {
                    if s.last_error.is_empty() {
                        s.last_error = val.as_str().unwrap_or("").to_string();
                    }
                }
                "agent_ingested_at" if s.agent_ingested_at.is_empty() => {
                    s.agent_ingested_at = val.as_str().unwrap_or("").to_string();
                }
                _ => {}
            }
        }

        s.normalize();
        Ok(s)
    }
}

// ---------------------------------------------------------------------------
// Factory functions
// ---------------------------------------------------------------------------

/// Build the default "agent managed with fallback" policy for a runner.
pub fn agent_managed_with_fallback_policy(runner: &str, expect_pr: bool) -> Policy {
    Policy {
        name: POLICY_AGENT_MANAGED_WITH_FALLBACK.into(),
        version: POLICY_VERSION,
        expect_commit: true,
        expect_push: true,
        expect_pr,
        allow_fallback: true,
        eligible_runners: match runner {
            r if !r.is_empty() => vec![r.to_string()],
            _ => vec!["worker".into(), "fixer".into(), "planner".into()],
        },
    }
}

/// Create a new lifecycle state from a policy and branch info.
pub fn new_state(policy: &Policy, branch: &str, base_branch: &str) -> State {
    let branch = branch.trim().to_string();
    let base_branch = base_branch.trim().to_string();
    State {
        policy: policy.name.clone(),
        policy_version: policy.version,
        branch: branch.clone(),
        base_branch: base_branch.clone(),
        planned_branch: branch.clone(),
        planned_base_branch: base_branch.clone(),
        active_branch: branch,
        active_base_branch: base_branch,
        branch_provenance: BRANCH_PROVENANCE_PLANNED.into(),
        actions: Actions {
            commit: ACTION_SOURCE_NONE.into(),
            push: ACTION_SOURCE_NONE.into(),
            pr: ACTION_SOURCE_NONE.into(),
        },
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

impl State {
    /// Normalize all fields, filling in defaults for empty values.
    pub fn normalize(&mut self) {
        if self.policy.is_empty() {
            self.policy = POLICY_AGENT_MANAGED_WITH_FALLBACK.into();
        }
        if self.policy_version == 0 {
            self.policy_version = POLICY_VERSION;
        }
        self.branch = self.branch.trim().to_string();
        self.base_branch = self.base_branch.trim().to_string();
        self.planned_branch = self.planned_branch.trim().to_string();
        self.planned_base_branch = self.planned_base_branch.trim().to_string();
        self.agent_branch = self.agent_branch.trim().to_string();
        self.agent_base_branch = self.agent_base_branch.trim().to_string();
        self.active_branch = self.active_branch.trim().to_string();
        self.active_base_branch = self.active_base_branch.trim().to_string();
        self.branch_provenance = self.branch_provenance.trim().to_string();

        if self.branch.is_empty() {
            self.branch = first_non_empty(&[&self.active_branch, &self.planned_branch]);
        }
        if self.base_branch.is_empty() {
            self.base_branch = first_non_empty(&[&self.active_base_branch, &self.planned_base_branch]);
        }
        if self.planned_branch.is_empty() {
            self.planned_branch = self.branch.clone();
        }
        if self.planned_base_branch.is_empty() {
            self.planned_base_branch = self.base_branch.clone();
        }
        if self.active_branch.is_empty() {
            self.active_branch = first_non_empty(&[&self.agent_branch, &self.branch]);
        }
        if self.active_base_branch.is_empty() {
            self.active_base_branch = first_non_empty(&[&self.agent_base_branch, &self.base_branch]);
        }
        if self.branch_provenance.is_empty() {
            if !self.planned_branch.is_empty()
                && !self.active_branch.is_empty()
                && self.planned_branch != self.active_branch
            {
                self.branch_provenance = BRANCH_PROVENANCE_AGENT_MIGRATED.into();
            } else {
                self.branch_provenance = BRANCH_PROVENANCE_PLANNED.into();
            }
        }
        self.commit_shas = compact_strings(&self.commit_shas);
        if self.actions.commit.is_empty() {
            self.actions.commit = action_source_from_bool(!self.commit_shas.is_empty());
        }
        if self.actions.push.is_empty() {
            self.actions.push = action_source_from_bool(self.pushed);
        }
        if self.actions.pr.is_empty() {
            self.actions.pr = action_source_from_bool(self.pr_number > 0 || !self.pr_url.trim().is_empty());
        }
    }

    /// Merge agent lifecycle state into this state.
    pub fn merge_agent(&mut self, agent: &State, ingested_at: &str) {
        if agent.policy.is_empty() && self.policy.is_empty() {
            // nothing useful
        }
        if self.policy.is_empty() {
            self.policy.clone_from(&agent.policy);
        }
        if self.policy_version == 0 {
            self.policy_version = agent.policy_version;
        }
        if self.branch.is_empty() && !agent.branch.is_empty() {
            self.branch = agent.branch.clone();
        }
        if self.base_branch.is_empty() && !agent.base_branch.is_empty() {
            self.base_branch = agent.base_branch.clone();
        }
        if self.planned_branch.is_empty() {
            self.planned_branch = first_non_empty(&[&agent.planned_branch, &agent.branch]);
        }
        if self.planned_base_branch.is_empty() {
            self.planned_base_branch = first_non_empty(&[&agent.planned_base_branch, &agent.base_branch]);
        }
        let agent_branch = first_non_empty(&[&agent.agent_branch, &agent.branch]);
        if !agent_branch.is_empty() {
            self.agent_branch = agent_branch;
        }
        let agent_base_branch = first_non_empty(&[&agent.agent_base_branch, &agent.base_branch]);
        if !agent_base_branch.is_empty() {
            self.agent_base_branch = agent_base_branch;
        }
        if self.active_branch.is_empty() {
            self.active_branch = first_non_empty(&[&agent.active_branch, &agent.branch]);
        }
        if self.active_base_branch.is_empty() {
            self.active_base_branch = first_non_empty(&[&agent.active_base_branch, &agent.base_branch]);
        }
        if !agent.branch_provenance.is_empty() {
            self.branch_provenance = agent.branch_provenance.clone();
        }
        self.commit_shas = append_unique(&self.commit_shas, &agent.commit_shas);
        if agent.pushed {
            self.pushed = true;
            if self.actions.push != ACTION_SOURCE_FALLBACK {
                self.actions.push = source_or(&agent.actions.push, ACTION_SOURCE_AGENT);
            }
        }
        if agent.pr_number > 0 {
            self.pr_number = agent.pr_number;
        }
        if !agent.pr_url.trim().is_empty() {
            self.pr_url = agent.pr_url.clone();
        }
        if agent.pr_adopted {
            self.pr_adopted = true;
        }
        if !agent.commit_shas.is_empty() && self.actions.commit != ACTION_SOURCE_FALLBACK {
            self.actions.commit = source_or(&agent.actions.commit, ACTION_SOURCE_AGENT);
        }
        if (agent.pr_number > 0 || !agent.pr_url.is_empty()) && self.actions.pr != ACTION_SOURCE_FALLBACK {
            self.actions.pr = source_or(&agent.actions.pr, ACTION_SOURCE_AGENT);
        }
        if !ingested_at.is_empty() {
            self.agent_ingested_at = ingested_at.to_string();
        }
        self.normalize();
    }

    /// Record an agent branch in the lifecycle state.
    pub fn record_agent_branch(&mut self, branch: &str, base_branch: &str) {
        let branch = branch.trim().to_string();
        let base = base_branch.trim().to_string();
        if !branch.is_empty() {
            self.agent_branch = branch;
        }
        if !base.is_empty() {
            self.agent_base_branch = base;
        }
        self.normalize();
    }

    /// Record the active branch after an agent run.
    pub fn set_active_branch(&mut self, branch: &str, base_branch: &str, provenance: &str) {
        let branch = branch.trim().to_string();
        let base = base_branch.trim().to_string();
        let prov = provenance.trim().to_string();
        if !branch.is_empty() {
            self.active_branch = branch;
        }
        if !base.is_empty() {
            self.active_base_branch = base;
        }
        if !prov.is_empty() {
            self.branch_provenance = prov;
        }
        self.normalize();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn action_source_from_bool(done: bool) -> String {
    if done {
        ACTION_SOURCE_AGENT.into()
    } else {
        ACTION_SOURCE_NONE.into()
    }
}

fn source_or(value: &str, fallback: &str) -> String {
    let v = value.trim();
    if !v.is_empty() && v != ACTION_SOURCE_NONE {
        v.to_string()
    } else {
        fallback.to_string()
    }
}

fn first_non_empty(values: &[&str]) -> String {
    for v in values {
        let t = v.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    String::new()
}

fn compact_strings(values: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for v in values {
        let t = v.trim().to_string();
        if t.is_empty() || seen.contains(&t) {
            continue;
        }
        seen.insert(t.clone());
        out.push(t);
    }
    out
}

fn append_unique(dst: &[String], values: &[String]) -> Vec<String> {
    let mut result: Vec<String> = dst.to_vec();
    let mut seen: HashSet<&str> = dst.iter().map(|s| s.as_str()).collect();
    for v in values {
        let t = v.trim();
        if t.is_empty() || seen.contains(t) {
            continue;
        }
        seen.insert(t);
        result.push(t.to_string());
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state() {
        let policy = agent_managed_with_fallback_policy("worker", true);
        let s = new_state(&policy, "feat/login", "main");
        assert_eq!(s.policy, POLICY_AGENT_MANAGED_WITH_FALLBACK);
        assert_eq!(s.branch, "feat/login");
        assert_eq!(s.base_branch, "main");
        assert_eq!(s.branch_provenance, BRANCH_PROVENANCE_PLANNED);
        assert_eq!(s.actions.commit, ACTION_SOURCE_NONE);
    }

    #[test]
    fn test_normalize_fills_defaults() {
        let mut s = State { branch: "feat/x".into(), base_branch: "main".into(), ..Default::default() };
        s.normalize();
        assert_eq!(s.policy, POLICY_AGENT_MANAGED_WITH_FALLBACK);
        assert_eq!(s.policy_version, POLICY_VERSION);
        assert_eq!(s.planned_branch, "feat/x");
        assert_eq!(s.active_branch, "feat/x");
    }

    #[test]
    fn test_merge_agent_commits() {
        let policy = agent_managed_with_fallback_policy("worker", true);
        let mut state = new_state(&policy, "feat/login", "main");

        let agent = State {
            commit_shas: vec!["abc123".into()],
            pushed: true,
            actions: Actions {
                commit: ACTION_SOURCE_AGENT.into(),
                push: ACTION_SOURCE_AGENT.into(),
                pr: ACTION_SOURCE_NONE.into(),
            },
            ..Default::default()
        };

        state.merge_agent(&agent, "2026-06-22T12:00:00Z");
        assert!(state.commit_shas.contains(&"abc123".into()));
        assert!(state.pushed);
        assert_eq!(state.actions.commit, ACTION_SOURCE_AGENT);
        assert_eq!(state.actions.push, ACTION_SOURCE_AGENT);
    }

    #[test]
    fn test_merge_agent_preserves_fallback() {
        let policy = agent_managed_with_fallback_policy("worker", true);
        let mut state = new_state(&policy, "feat/login", "main");
        state.actions.push = ACTION_SOURCE_FALLBACK.into();

        let agent = State {
            pushed: true,
            commit_shas: vec!["abc".into()],
            actions: Actions {
                commit: ACTION_SOURCE_AGENT.into(),
                push: ACTION_SOURCE_AGENT.into(),
                pr: ACTION_SOURCE_NONE.into(),
            },
            ..Default::default()
        };

        state.merge_agent(&agent, "");
        // Fallback should NOT be overwritten
        assert_eq!(state.actions.push, ACTION_SOURCE_FALLBACK);
    }

    #[test]
    fn test_merge_agent_pr() {
        let policy = agent_managed_with_fallback_policy("worker", true);
        let mut state = new_state(&policy, "feat/login", "main");

        let agent = State {
            pr_number: 42,
            pr_url: "https://github.com/owner/repo/pull/42".into(),
            pr_adopted: true,
            actions: Actions { pr: ACTION_SOURCE_AGENT.into(), ..Default::default() },
            ..Default::default()
        };

        state.merge_agent(&agent, "");
        assert_eq!(state.pr_number, 42);
        assert!(state.pr_adopted);
        assert!(state.actions.pr == ACTION_SOURCE_AGENT);
    }

    #[test]
    fn test_branch_provenance_agent_migrated() {
        let mut s = State {
            planned_branch: "planned-branch".into(),
            active_branch: "agent-branch".into(),
            ..Default::default()
        };
        s.normalize();
        assert_eq!(s.branch_provenance, BRANCH_PROVENANCE_AGENT_MIGRATED);
    }

    #[test]
    fn test_record_agent_branch() {
        let mut s = new_state(&agent_managed_with_fallback_policy("worker", false), "planned", "main");
        s.record_agent_branch("agent-branch", "agent-base");
        assert_eq!(s.agent_branch, "agent-branch");
        assert_eq!(s.agent_base_branch, "agent-base");
    }

    #[test]
    fn test_set_active_branch() {
        let mut s = State::default();
        s.set_active_branch("active", "active-base", BRANCH_PROVENANCE_AGENT_MIGRATED);
        assert_eq!(s.active_branch, "active");
        assert_eq!(s.branch_provenance, BRANCH_PROVENANCE_AGENT_MIGRATED);
    }

    #[test]
    fn test_camel_case_deser() {
        let json = serde_json::json!({
            "policy": "agent_managed_with_fallback",
            "policyVersion": 1,
            "branch": "feat/x",
            "baseBranch": "main",
            "plannedBranch": "feat/x",
            "prNumber": 42,
            "commitShas": ["abc"],
            "actions": {"commit": "agent", "push": "none", "pr": "agent"}
        });
        let s = State::from_json_value(&json).unwrap();
        assert_eq!(s.branch, "feat/x");
        assert_eq!(s.pr_number, 42);
        assert_eq!(s.actions.commit, ACTION_SOURCE_AGENT);
    }

    #[test]
    fn test_action_source_from_bool() {
        assert_eq!(action_source_from_bool(true), ACTION_SOURCE_AGENT);
        assert_eq!(action_source_from_bool(false), ACTION_SOURCE_NONE);
    }

    #[test]
    fn test_source_or() {
        assert_eq!(source_or(ACTION_SOURCE_AGENT, "default"), ACTION_SOURCE_AGENT);
        assert_eq!(source_or(ACTION_SOURCE_NONE, "default"), "default");
    }

    #[test]
    fn test_compact_strings() {
        let v = vec!["a".into(), "b".into(), "a".into(), "".into()];
        let c = compact_strings(&v);
        assert_eq!(c.len(), 2);
        assert!(c.contains(&"a".into()));
    }

    #[test]
    fn test_append_unique() {
        let dst = vec!["a".into()];
        let vals = vec!["a".into(), "b".into()];
        let r = append_unique(&dst, &vals);
        assert_eq!(r.len(), 2);
    }
}
