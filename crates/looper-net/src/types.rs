use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Version constants
// ---------------------------------------------------------------------------

pub const PROTOCOL_CURRENT_VERSION: &str = "loopernet/v1";
pub const DEFAULT_LEASE_TTL_SECONDS: u64 = 30;
pub const HEARTBEAT_INTERVAL_SECONDS: u64 = 10;
pub fn validate_node_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 32 {
        return false;
    }
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}
pub const TARGET_LABEL_PREFIX: &str = "looper:target:";

// ---------------------------------------------------------------------------
// GitHub identity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubIdentity {
    #[serde(rename = "numericId")]
    pub numeric_id: i64,
    pub login: String,
}

// ---------------------------------------------------------------------------
// Reviewer project config (in capabilities)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReviewerProject {
    #[serde(rename = "projectId")]
    pub project_id: String,
    #[serde(rename = "includeDrafts", default)]
    pub include_drafts: bool,
    #[serde(rename = "requireReviewRequest", default = "default_true")]
    pub require_review_request: bool,
    #[serde(rename = "enableSelfReview", default)]
    pub enable_self_review: bool,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(rename = "labelMode", default)]
    pub label_mode: String,
}

fn default_true() -> bool { true }

// ---------------------------------------------------------------------------
// Node capabilities
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeCapabilities {
    pub roles: Vec<String>,
    #[serde(rename = "coordinatorEligible", default)]
    pub coordinator_eligible: bool,
    #[serde(rename = "routedProjects")]
    pub routed_projects: i64,
    #[serde(rename = "routedProjectIds", default)]
    pub routed_project_ids: Vec<String>,
    #[serde(rename = "reviewerProjects", default)]
    pub reviewer_projects: Vec<ReviewerProject>,
    #[serde(rename = "localProjects")]
    pub local_projects: i64,
    #[serde(rename = "dynamicLoad")]
    pub dynamic_load: i64,
    #[serde(rename = "identityDrift", default)]
    pub identity_drift: bool,
    #[serde(rename = "driftReason", default)]
    pub drift_reason: String,
}

// ---------------------------------------------------------------------------
// Join request / response
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "daemonVersion")]
    pub daemon_version: String,
    #[serde(rename = "joinKey")]
    pub join_key: String,
    #[serde(rename = "nodeName")]
    pub node_name: String,
    pub github: GitHubIdentity,
    #[serde(rename = "targetLabels", default)]
    pub target_labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    #[serde(rename = "networkId")]
    pub network_id: String,
    #[serde(rename = "nodeId")]
    pub node_id: String,
    #[serde(rename = "nodeToken")]
    pub node_token: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Heartbeat request / response
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatRequest {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "daemonVersion")]
    pub daemon_version: String,
    #[serde(rename = "nodeName")]
    pub node_name: String,
    pub github: GitHubIdentity,
    pub capabilities: NodeCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    #[serde(rename = "recordedAt")]
    pub recorded_at: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Membership & node status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Membership {
    #[serde(rename = "nodeId")]
    pub node_id: String,
    #[serde(rename = "nodeName")]
    pub node_name: String,
    #[serde(rename = "daemonVersion")]
    pub daemon_version: String,
    pub github: GitHubIdentity,
    pub capabilities: NodeCapabilities,
    #[serde(rename = "targetLabels", default)]
    pub target_labels: Vec<String>,
    #[serde(rename = "joinedAt")]
    pub joined_at: String,
    #[serde(rename = "lastHeartbeatAt", default)]
    pub last_heartbeat_at: Option<String>,
    #[serde(rename = "duplicateGithubIdentityWarning", default)]
    pub duplicate_github_identity_warning: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorLease {
    pub name: String,
    #[serde(rename = "holderNodeId")]
    pub holder_node_id: Option<String>,
    #[serde(rename = "fencingToken")]
    pub fencing_token: i64,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookStatus {
    #[serde(rename = "deliveriesReceived", default)]
    pub deliveries_received: i64,
    #[serde(rename = "lastDeliveryAt")]
    pub last_delivery_at: Option<String>,
    #[serde(rename = "lastDeliveryId")]
    pub last_delivery_id: Option<String>,
    #[serde(rename = "lastEvent")]
    pub last_event: Option<String>,
    #[serde(rename = "lastRepo")]
    pub last_repo: Option<String>,
    #[serde(rename = "eventSubscribers", default)]
    pub event_subscribers: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatusResponse {
    #[serde(rename = "networkId")]
    pub network_id: String,
    pub membership: Membership,
    pub memberships: Vec<Membership>,
    pub lease: Option<CoordinatorLease>,
    pub webhook: Option<WebhookStatus>,
    pub warnings: Vec<String>,
    #[serde(rename = "cloudReachable", default)]
    pub cloud_reachable: bool,
    #[serde(rename = "currentGithub")]
    pub current_github: Option<GitHubIdentity>,
    #[serde(rename = "identityDrift", default)]
    pub identity_drift: bool,
    #[serde(rename = "identityDriftReason", default)]
    pub identity_drift_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    #[serde(rename = "networkId")]
    pub network_id: String,
    pub lease: Option<CoordinatorLease>,
    pub memberships: Vec<Membership>,
    pub webhook: Option<WebhookStatus>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Audit envelope (SSE events)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEnvelope {
    pub event: String,
    pub actor: String,
    #[serde(rename = "occurredAt")]
    pub occurred_at: String,
    #[serde(rename = "networkId")]
    pub network_id: String,
    #[serde(rename = "nodeId")]
    pub node_id: Option<String>,
    #[serde(rename = "leaseName")]
    pub lease_name: Option<String>,
    #[serde(rename = "leaseToken")]
    pub lease_token: Option<i64>,
    pub payload: Option<serde_json::Value>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Lease operations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorLeaseAcquireRequest {
    #[serde(rename = "nodeId")]
    pub node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorLeaseRenewRequest {
    #[serde(rename = "fencingToken")]
    pub fencing_token: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorLeaseHandoffRequest {
    #[serde(rename = "fencingToken")]
    pub fencing_token: i64,
    #[serde(rename = "targetNodeId")]
    pub target_node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorLeaseRevalidateRequest {
    #[serde(rename = "fencingToken")]
    pub fencing_token: i64,
    pub url: String,
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FencingTokenRequest {
    #[serde(rename = "fencingToken")]
    pub fencing_token: i64,
}

// ---------------------------------------------------------------------------
// Webhook forwarding (routed through cloud)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookSecretResponse {
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookForwardRequest {
    #[serde(rename = "deliveryId")]
    pub delivery_id: String,
    pub event: String,
    pub payload: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Client-side local state
// ---------------------------------------------------------------------------

/// Persisted to `~/.looper/network.json` after joining a network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalState {
    pub url: String,
    #[serde(rename = "networkId")]
    pub network_id: String,
    #[serde(rename = "nodeId")]
    pub node_id: String,
    #[serde(rename = "nodeName")]
    pub node_name: String,
    #[serde(rename = "nodeToken")]
    pub node_token: String,
    pub github: GitHubIdentity,
}


// ---------------------------------------------------------------------------
// Claim policy types
// ---------------------------------------------------------------------------

/// Whether a project participates in network-distributed work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum NetworkMode {
    #[default]
    Off,
    Routed,
}


/// How the local node's GitHub identity was matched against a PR assignee/reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum MatchMode {
    #[default]
    None,
    Numeric,
    LoginFallback,
}


/// Result of evaluating whether a node should claim a work item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimDecision {
    pub allowed: bool,
    pub reason: String,
    #[serde(rename = "matchMode")]
    pub match_mode: MatchMode,
    #[serde(rename = "targetLabel")]
    pub target_label: String,
}

/// Policy for a single project (used by claim evaluation).
#[derive(Debug, Clone)]
pub struct ProjectPolicy {
    pub mode: NetworkMode,
    pub node_name: String,
    pub github_login: String,
    pub github_user_id: i64,
}

// ---------------------------------------------------------------------------
// Cloud server config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NetConfig {
    pub listen_addr: String,
    pub db_path: String,
    pub admin_token: String,
    pub network_id: String,
    pub protocol_version: String,
    pub minimum_daemon_version: Option<String>,
    pub lease_ttl_seconds: u64,
    pub server_version: String,
    pub advertise_url: Option<String>,
}

impl Default for NetConfig {
    fn default() -> Self {
        NetConfig {
            listen_addr: "127.0.0.1:8089".to_string(),
            db_path: String::new(),
            admin_token: String::new(),
            network_id: String::new(),
            protocol_version: PROTOCOL_CURRENT_VERSION.to_string(),
            minimum_daemon_version: None,
            lease_ttl_seconds: DEFAULT_LEASE_TTL_SECONDS,
            server_version: String::new(),
            advertise_url: None,
        }
    }
}

// ---------------------------------------------------------------------------
// API error response from cloud server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub message: String,
}

// ---------------------------------------------------------------------------
// Join key creation response
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinKeyResponse {
    #[serde(rename = "joinKey")]
    pub join_key: String,
}

// ---------------------------------------------------------------------------
// Helper: Target label parsing
// ---------------------------------------------------------------------------

/// Extract the node name from a `looper:target:<name>` label.
pub fn parse_target_label(label: &str) -> Option<&str> {
    label.strip_prefix(TARGET_LABEL_PREFIX)
        .filter(|name| !name.is_empty())
}

/// Build a target label for a given node name.
pub fn target_label_for_node(node_name: &str) -> String {
    format!("{}{}", TARGET_LABEL_PREFIX, node_name)
}

/// Collect all target labels from a label list.
pub fn collect_target_labels(labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .filter(|l| l.starts_with(TARGET_LABEL_PREFIX))
        .cloned()
        .collect()
}

/// Check if a set of labels has exactly one target label for the given node.
pub fn has_exact_target(labels: &[String], node_name: &str) -> bool {
    let _expected = target_label_for_node(node_name);
    let targets: Vec<&str> = labels
        .iter()
        .filter_map(|l| l.strip_prefix(TARGET_LABEL_PREFIX))
        .collect();
    targets.len() == 1 && targets[0] == node_name
}

/// Build a label diff to make labels have exactly one target for the given node.
pub fn plan_exact_target(labels: &mut Vec<String>, node_name: &str) {
    let expected = target_label_for_node(node_name);
    labels.retain(|l| !l.starts_with(TARGET_LABEL_PREFIX));
    labels.push(expected);
}

// ---------------------------------------------------------------------------
// Version comparison
// ---------------------------------------------------------------------------

/// Simple semver comparison: returns true if `version >= minimum`.
/// Strips leading `v` and pre-release/build-metadata suffixes.
pub fn is_version_at_least(version: &str, minimum: &str) -> bool {
    fn parse(s: &str) -> Option<(u64, u64, u64)> {
        let s = s.trim_start_matches('v');
        let s = s.split(['-', '+']).next()?;
        let mut parts = s.splitn(3, '.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        Some((major, minor, patch))
    }
    match (parse(version), parse(minimum)) {
        (Some(v), Some(m)) => (v.0 > m.0) || (v.0 == m.0 && v.1 > m.1) || (v.0 == m.0 && v.1 == m.1 && v.2 >= m.2),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Identity drift detection
// ---------------------------------------------------------------------------

pub fn detect_identity_drift(stored: &GitHubIdentity, current: &GitHubIdentity) -> (bool, String) {
    if stored.numeric_id > 0 && current.numeric_id > 0 && stored.numeric_id != current.numeric_id {
        return (
            true,
            format!(
                "GitHub numeric ID changed from {} to {}",
                stored.numeric_id, current.numeric_id
            ),
        );
    }
    if stored.login.to_lowercase() != current.login.to_lowercase() {
        return (
            true,
            format!(
                "GitHub login changed from {} to {}",
                stored.login, current.login
            ),
        );
    }
    (false, String::new())
}

impl Default for LocalState {
    fn default() -> Self {
        LocalState {
            url: String::new(),
            network_id: String::new(),
            node_id: String::new(),
            node_name: String::new(),
            node_token: String::new(),
            github: GitHubIdentity {
                numeric_id: 0,
                login: String::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_target_label() {
        assert_eq!(parse_target_label("looper:target:my-node"), Some("my-node"));
        assert_eq!(parse_target_label("looper:target:"), None);
        assert_eq!(parse_target_label("looper:other:my-node"), None);
        assert_eq!(parse_target_label(""), None);
    }

    #[test]
    fn test_target_label_for_node() {
        assert_eq!(target_label_for_node("my-node"), "looper:target:my-node");
        assert_eq!(target_label_for_node(""), "looper:target:");
    }

    #[test]
    fn test_collect_target_labels() {
        let labels = vec![
            "looper:target:node-a".to_string(),
            "bug".to_string(),
            "looper:target:node-b".to_string(),
        ];
        let targets = collect_target_labels(&labels);
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&"looper:target:node-a".to_string()));
        assert!(targets.contains(&"looper:target:node-b".to_string()));
    }

    #[test]
    fn test_has_exact_target() {
        let labels = vec!["looper:target:my-node".to_string(), "bug".to_string()];
        assert!(has_exact_target(&labels, "my-node"));
        assert!(!has_exact_target(&labels, "other-node"));

        let multi = vec![
            "looper:target:node-a".to_string(),
            "looper:target:node-b".to_string(),
        ];
        assert!(!has_exact_target(&multi, "node-a"));
    }

    #[test]
    fn test_plan_exact_target() {
        let mut labels = vec!["bug".to_string(), "looper:target:old".to_string()];
        plan_exact_target(&mut labels, "new-node");
        assert_eq!(labels, vec!["bug".to_string(), "looper:target:new-node".to_string()]);
    }

    #[test]
    fn test_version_comparison() {
        assert!(is_version_at_least("1.2.3", "1.2.3"));
        assert!(is_version_at_least("1.2.4", "1.2.3"));
        assert!(!is_version_at_least("1.2.2", "1.2.3"));
        assert!(is_version_at_least("1.3.0", "1.2.99"));
        assert!(!is_version_at_least("0.9.0", "1.0.0"));
        assert!(is_version_at_least("v1.2.3", "1.2.3"));
        assert!(is_version_at_least("1.2.3-beta.1", "1.2.3"));
        assert!(!is_version_at_least("1.2.3-beta.1", "1.2.4"));
        assert!(is_version_at_least("1.2.3+build.1", "1.2.3"));
        assert!(is_version_at_least("v2.0.0-alpha", "1.9.9"));
    }

    #[test]
    fn test_identity_drift() {
        let stored = GitHubIdentity { numeric_id: 100, login: "alice".to_string() };
        let same = GitHubIdentity { numeric_id: 100, login: "Alice".to_string() };
        let diff_id = GitHubIdentity { numeric_id: 200, login: "alice".to_string() };
        let diff_login = GitHubIdentity { numeric_id: 100, login: "bob".to_string() };

        assert_eq!(detect_identity_drift(&stored, &same), (false, String::new()));
        let (drifted, _) = detect_identity_drift(&stored, &diff_id);
        assert!(drifted);
        let (drifted, _) = detect_identity_drift(&stored, &diff_login);
        assert!(drifted);

        // Zero numeric ID = unavailable, not drift
        let unavailable = GitHubIdentity { numeric_id: 0, login: "alice".to_string() };
        assert_eq!(detect_identity_drift(&stored, &unavailable), (false, String::new()));
    }

    #[test]
    fn test_node_name_validation() {
        assert!(validate_node_name("my-node-1"));
        assert!(validate_node_name("a"));
        assert!(validate_node_name("abc123.def_ghi-jkl"));
        assert!(!validate_node_name(""));
        assert!(!validate_node_name(&"a".repeat(33)));
        assert!(!validate_node_name("node:name"));
        assert!(!validate_node_name("name with spaces"));
    }
}
