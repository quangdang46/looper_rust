use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::binaries::BuiltBinaries;

// ---------------------------------------------------------------------------
// Environment variable names (exported for binary use)
// ---------------------------------------------------------------------------

/// Mode env-var — controls how fake-gh responds ("strict", "record", "replay").
pub const ENV_FAKE_GH_MODE: &str = "LOOPER_E2E_FAKE_GH_MODE";
/// Artifact directory.
pub const ENV_FAKE_GH_ARTIFACT_DIR: &str = "LOOPER_E2E_FAKE_GH_ARTIFACT_DIR";
/// Path to the JSON schema file that defines API allowlists.
pub const ENV_FAKE_GH_SCHEMA_PATH: &str = "LOOPER_E2E_FAKE_GH_SCHEMA_PATH";
/// Path to the fake-gh state JSON file (persistent across invocations).
pub const ENV_FAKE_GH_STATE_PATH: &str = "LOOPER_E2E_FAKE_GH_STATE_PATH";
/// Path to a JSONL record of all invocations.
pub const ENV_FAKE_GH_RECORD_PATH: &str = "LOOPER_E2E_FAKE_GH_RECORD_PATH";
/// Path to a git binary for operations that need it.
pub const ENV_FAKE_GH_GIT_PATH: &str = "LOOPER_E2E_FAKE_GH_GIT_PATH";

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// JSON-schema that defines which JSON fields are allowed for each API route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GHSchema {
    /// Map of route → list of allowed JSON field names.
    #[serde(rename = "jsonFieldAllowlist")]
    pub json_field_allowlist: HashMap<String, Vec<String>>,
}

// ---------------------------------------------------------------------------
// FakeGH
// ---------------------------------------------------------------------------

/// Handle wrapping the `fake-gh` binary and its associated data files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakeGH {
    /// Path to the `fake-gh` binary.
    pub path: PathBuf,
    /// Operating mode ("strict", "record", "replay").
    pub mode: String,
    /// Path to a git binary.
    pub git_path: String,
    /// Artifact directory where schema, state, and records live.
    pub artifact_dir: PathBuf,
    /// Path to the JSON schema file.
    pub schema_path: PathBuf,
    /// Path to the state JSON file.
    pub state_path: PathBuf,
    /// Path to the JSONL invocation record.
    pub record_path: PathBuf,
}

impl FakeGH {
    /// Create a new [`FakeGH`] from binaries and a schema.
    ///
    /// Creates the artifact directory tree and writes the schema file.
    ///
    /// # Panics
    /// Panics on I/O errors.
    pub fn new(bins: &BuiltBinaries, schema: GHSchema) -> Self {
        let root = std::env::temp_dir().join(format!("fake-gh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create fake-gh root");

        let schema_path = root.join("schema.json");
        let payload =
            serde_json::to_string_pretty(&schema).expect("serialise fake-gh schema");
        std::fs::write(&schema_path, &payload).expect("write fake-gh schema");

        Self {
            path: bins.fake_gh_path.clone(),
            mode: "strict".to_string(),
            git_path: "git".to_string(),
            artifact_dir: root.clone(),
            schema_path,
            state_path: root.join("state.json"),
            record_path: root.join("record.jsonl"),
        }
    }

    /// Build an environment-variable map that the `fake-gh` binary reads.
    pub fn env_map(&self) -> HashMap<String, String> {
        let mode = if self.mode.is_empty() { "strict" } else { &self.mode };
        let mut m = HashMap::new();
        m.insert(ENV_FAKE_GH_MODE.to_string(), mode.to_string());
        m.insert(
            ENV_FAKE_GH_GIT_PATH.to_string(),
            if self.git_path.is_empty() { "git".to_string() } else { self.git_path.clone() },
        );
        m.insert(
            ENV_FAKE_GH_ARTIFACT_DIR.to_string(),
            self.artifact_dir.to_string_lossy().to_string(),
        );
        m.insert(
            ENV_FAKE_GH_SCHEMA_PATH.to_string(),
            self.schema_path.to_string_lossy().to_string(),
        );
        m.insert(
            ENV_FAKE_GH_STATE_PATH.to_string(),
            self.state_path.to_string_lossy().to_string(),
        );
        m.insert(
            ENV_FAKE_GH_RECORD_PATH.to_string(),
            self.record_path.to_string_lossy().to_string(),
        );
        m
    }

    /// Serialise `state` and write it to `self.state_path`.
    ///
    /// # Panics
    /// Panics if the parent directory cannot be created, serialisation
    /// fails, or the write fails.
    pub fn write_state(&self, state: &GHState) {
        if let Some(parent) = self.state_path.parent() {
            std::fs::create_dir_all(parent).expect("create fake-gh state dir");
        }
        let payload =
            serde_json::to_string_pretty(state).expect("serialise fake-gh state");
        std::fs::write(&self.state_path, &payload).expect("write fake-gh state");
    }
}

// ---------------------------------------------------------------------------
// GH types
// ---------------------------------------------------------------------------

/// A single comment on a review thread.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GHThreadComment {
    /// Comment ID.
    pub id: String,
    /// Comment body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Author login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Creation timestamp (ISO-8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Last-update timestamp (ISO-8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// File path the comment refers to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Line number the comment refers to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    /// OID of the commit the comment was originally on.
    #[serde(rename = "originalCommitOid", default, skip_serializing_if = "Option::is_none")]
    pub original_commit_oid: Option<String>,
    /// OID of the current commit.
    #[serde(rename = "commitOid", default, skip_serializing_if = "Option::is_none")]
    pub commit_oid: Option<String>,
    /// URL of the comment on GitHub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A review thread (group of comments on the same line).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GHThread {
    /// Thread ID.
    pub id: String,
    /// Whether the thread has been resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_resolved: Option<bool>,
    /// File path the thread refers to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Line number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    /// Comments in this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<GHThreadComment>>,
}

/// A pull request as represented in the fake-gh state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GHPullRequest {
    /// PR number.
    pub number: i64,
    /// Repository (e.g. "owner/repo").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// PR title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// PR body / description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// GitHub URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// PR state ("open", "closed", "merged").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Creation timestamp (ISO-8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Last-update timestamp (ISO-8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Close timestamp (ISO-8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    /// Whether this PR is a draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    /// Review decision ("APPROVED", "CHANGES_REQUESTED", "REVIEW_REQUIRED").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    /// Labels applied to the PR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Name of the head branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref_name: Option<String>,
    /// Name of the base branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref_name: Option<String>,
    /// Full head ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    /// Full base ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    /// SHA of the head commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// SHA of the base commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    /// Path to a git directory (for test repos).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_dir: Option<String>,
    /// PR author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Author association (e.g. "OWNER", "MEMBER").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_association: Option<String>,
    /// Users who have been asked to review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_requests: Option<Vec<String>>,
    /// Issue comments (free-form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_comments: Option<Vec<serde_json::Value>>,
    /// Reviews (free-form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviews: Option<Vec<serde_json::Value>>,
    /// Status check rollup (free-form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_check_rollup: Option<Vec<serde_json::Value>>,
    /// Merge state status ("clean", "dirty", etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_state_status: Option<String>,
    /// Whether the PR is mergeable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mergeable: Option<bool>,
    /// Mergeable state string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mergeable_state: Option<String>,
    /// Merge timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<String>,
    /// Auto-merge configuration (free-form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge: Option<serde_json::Value>,
    /// Check runs (free-form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_runs: Option<Vec<serde_json::Value>>,
    /// Review threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<Vec<GHThread>>,
}

/// Complete state of the fake-gh server, serialised to JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GHState {
    /// Pre-defined command responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<serde_json::Value>,
    /// Route definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<serde_json::Value>,
    /// GraphQL responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql: Option<serde_json::Value>,
    /// The current authenticated user's login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_user_login: Option<String>,
    /// Pull request state keyed by a canonical identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<HashMap<String, GHPullRequest>>,
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fake_gh_roundtrip() {
        let schema = GHSchema {
            json_field_allowlist: HashMap::from([(
                "pull-request".to_string(),
                vec!["title".to_string(), "body".to_string()],
            )]),
        };
        let bins = BuiltBinaries {
            looper_path: "/bin/looper".into(),
            looperd_path: "/bin/looperd".into(),
            fake_agent_path: "/bin/fake-agent".into(),
            fake_gh_path: "/bin/fake-gh".into(),
            fake_osascript_path: "/bin/fake-osascript".into(),
        };
        let fgh = FakeGH::new(&bins, schema);
        assert!(fgh.schema_path.exists());
        assert!(fgh.state_path.parent().unwrap().exists());

        let state = GHState {
            commands: None,
            routes: None,
            graphql: None,
            current_user_login: Some("e2e-user".into()),
            pull_requests: None,
        };
        fgh.write_state(&state);
        assert!(fgh.state_path.exists());
    }

    #[test]
    fn test_fake_gh_env_map() {
        let schema = GHSchema {
            json_field_allowlist: HashMap::new(),
        };
        let bins = BuiltBinaries {
            looper_path: "/bin/looper".into(),
            looperd_path: "/bin/looperd".into(),
            fake_agent_path: "/bin/fake-agent".into(),
            fake_gh_path: "/bin/fake-gh".into(),
            fake_osascript_path: "/bin/fake-osascript".into(),
        };
        let fgh = FakeGH::new(&bins, schema);
        let env = fgh.env_map();
        assert!(env.contains_key(ENV_FAKE_GH_MODE));
        assert!(env.contains_key(ENV_FAKE_GH_ARTIFACT_DIR));
        assert!(env.contains_key(ENV_FAKE_GH_STATE_PATH));
    }
}
