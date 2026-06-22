use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// ---------------------------------------------------------------------------
/// Record types — each maps 1:1 to a SQL row
/// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub repo_path: String,
    pub base_branch: Option<String>,
    pub archived: bool,
    pub metadata_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopRecord {
    pub id: String,
    pub seq: i64,
    pub project_id: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub repo: Option<String>,
    pub pr_number: Option<i64>,
    pub status: String,
    pub config_json: Option<String>,
    pub metadata_json: Option<String>,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub loop_id: String,
    pub status: String,
    pub current_step: Option<String>,
    pub last_completed_step: Option<String>,
    pub checkpoint_json: Option<String>,
    pub summary: Option<String>,
    pub error_message: Option<String>,
    pub started_at: String,
    pub last_heartbeat_at: Option<String>,
    pub ended_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExecutionRecord {
    pub id: String,
    pub project_id: Option<String>,
    pub loop_id: Option<String>,
    pub run_id: Option<String>,
    pub vendor: String,
    pub status: String,
    pub pid: Option<i64>,
    pub command_json: Option<String>,
    pub cwd: Option<String>,
    pub summary: Option<String>,
    pub parse_status: Option<String>,
    pub completion_signal: Option<String>,
    pub heartbeat_count: i64,
    pub last_heartbeat_at: Option<String>,
    pub output_json: Option<String>,
    pub error_message: Option<String>,
    pub native_session_id: Option<String>,
    pub native_resume_mode: Option<String>,
    pub native_resume_status: Option<String>,
    pub native_resume_error: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub metadata_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestSnapshotRecord {
    pub id: String,
    pub project_id: String,
    pub repo: String,
    pub pr_number: i64,
    pub head_sha: String,
    pub base_sha: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub author: Option<String>,
    pub diff_ref: Option<String>,
    pub checks_summary: Option<String>,
    pub unresolved_thread_count: Option<i64>,
    pub review_state: Option<String>,
    pub payload_json: Option<String>,
    pub captured_at: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLogRecord {
    pub id: String,
    pub event_type: String,
    pub project_id: Option<String>,
    pub loop_id: Option<String>,
    pub run_id: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub actor_type: Option<String>,
    pub actor_id: Option<String>,
    pub actor_display_name: Option<String>,
    pub payload_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockRecord {
    pub key: String,
    pub owner: String,
    pub reason: Option<String>,
    pub expires_at: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItemRecord {
    pub id: String,
    pub project_id: Option<String>,
    pub loop_id: Option<String>,
    #[serde(rename = "type")]
    pub r#type: String,
    pub target_type: String,
    pub target_id: String,
    pub repo: Option<String>,
    pub pr_number: Option<i64>,
    pub dedupe_key: String,
    pub priority: i64,
    pub status: String,
    pub available_at: String,
    pub attempts: i64,
    pub max_attempts: i64,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub lock_key: Option<String>,
    pub payload_json: Option<String>,
    pub last_error: Option<String>,
    pub last_error_kind: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueStats {
    pub total_queued: i64,
    pub eligible_queued: i64,
    pub blocked_by_terminal_or_paused_loop: i64,
    pub blocked_by_lock_key: i64,
    pub blocked_by_reviewer_fixer_dependency: i64,
    pub scheduled_for_future: i64,
    pub stale_queued: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueMarkRetryInput {
    pub id: String,
    pub available_at: String,
    pub attempts: i64,
    pub error_message: Option<String>,
    pub error_kind: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueFailInput {
    pub id: String,
    pub attempts: i64,
    pub finished_at: String,
    pub error_message: Option<String>,
    pub error_kind: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRecord {
    pub id: String,
    pub project_id: Option<String>,
    pub loop_id: Option<String>,
    pub run_id: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub channel: String,
    pub level: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub body: String,
    pub status: String,
    pub dedupe_key: Option<String>,
    pub error_message: Option<String>,
    pub payload_json: Option<String>,
    pub sent_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeRecord {
    pub id: String,
    pub project_id: String,
    pub repo_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub base_branch: Option<String>,
    pub status: String,
    pub head_sha: Option<String>,
    pub metadata_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub cleaned_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookForwarderRecord {
    pub repo: String,
    pub pid: i64,
    pub process_start: i64,
    pub fingerprint: String,
    pub endpoint: String,
    pub events: String,
    pub gh_path: String,
    pub daemon_id: String,
    pub spawned_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookTunnelHookRecord {
    pub repo: String,
    pub hook_id: i64,
    pub managed_url: String,
    pub secret_ref: String,
    pub last_ping_at: Option<i64>,
    pub consecutive_disables: i64,
    pub last_disable_at: Option<i64>,
    pub orphaned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Input for the EventLog.Append function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendInput {
    pub event_type: String,
    pub project_id: Option<String>,
    pub loop_id: Option<String>,
    pub run_id: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub actor_type: Option<String>,
    pub actor_id: Option<String>,
    pub actor_display_name: Option<String>,
    pub payload_json: Option<String>,
    pub created_at: Option<String>,
    pub id: Option<String>,
}

impl AppendInput {
    pub fn new(event_type: impl Into<String>) -> Self {
        Self {
            event_type: event_type.into(),
            project_id: None,
            loop_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            correlation_id: None,
            causation_id: None,
            actor_type: None,
            actor_id: None,
            actor_display_name: None,
            payload_json: None,
            created_at: None,
            id: None,
        }
    }
}

/// Helper type for counts-by-status queries.
pub type StatusCountMap = HashMap<String, i64>;

/// Helper type for counts-by-type-and-status queries.
pub type TypeStatusCountMap = HashMap<String, HashMap<String, i64>>;
