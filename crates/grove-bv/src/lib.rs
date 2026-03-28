use grove_types::{BeadId, BeadPriority, Timestamp};
use serde::{Deserialize, de::Error as DeError};
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    process::Command,
};

pub const CRATE_PURPOSE: &str = "Integration boundary for beads_viewer (bv).";

pub trait BvClient {
    fn triage(&self) -> Result<BvTriageOutput, BvError>;
    fn next(&self) -> Result<BvNextOutput, BvError>;
    fn plan(&self) -> Result<BvPlanOutput, BvError>;
    fn insights(&self) -> Result<BvInsightsOutput, BvError>;
    fn priority_audit(&self) -> Result<BvPriorityAuditOutput, BvError>;
    fn alerts(&self) -> Result<BvAlertsOutput, BvError>;
    fn capability(&self) -> Result<BvCapability, BvError>;
}

#[derive(Debug, Clone)]
pub struct CliBvClient {
    bv_bin: String,
    working_dir: PathBuf,
}

impl CliBvClient {
    #[must_use]
    pub fn new(bv_bin: impl Into<String>, working_dir: impl Into<PathBuf>) -> Self {
        Self {
            bv_bin: bv_bin.into(),
            working_dir: working_dir.into(),
        }
    }

    #[must_use]
    pub fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    fn run(&self, args: &[&str]) -> Result<CommandOutput, BvError> {
        let output = Command::new(&self.bv_bin)
            .args(args)
            .current_dir(&self.working_dir)
            .output()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    BvError::NotFound {
                        path: self.bv_bin.clone(),
                    }
                } else {
                    BvError::Io(source)
                }
            })?;

        let stdout = String::from_utf8(output.stdout).map_err(BvError::Utf8)?;
        let stderr = String::from_utf8(output.stderr).map_err(BvError::Utf8)?;

        if output.status.success() {
            Ok(CommandOutput { stdout, stderr })
        } else {
            Err(BvError::CommandFailed {
                command: format_command(&self.bv_bin, args),
                code: output.status.code(),
                stdout,
                stderr,
            })
        }
    }
}

impl BvClient for CliBvClient {
    fn triage(&self) -> Result<BvTriageOutput, BvError> {
        let output = self.run(&["--robot-triage"])?;
        parse_triage_output(&output.stdout).map_err(|source| BvError::ParseError {
            command: format_command(&self.bv_bin, &["--robot-triage"]),
            source,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn next(&self) -> Result<BvNextOutput, BvError> {
        let output = self.run(&["--robot-next"])?;
        parse_next_output(&output.stdout).map_err(|source| BvError::ParseError {
            command: format_command(&self.bv_bin, &["--robot-next"]),
            source,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn plan(&self) -> Result<BvPlanOutput, BvError> {
        let output = self.run(&["--robot-plan"])?;
        parse_plan_output(&output.stdout).map_err(|source| BvError::ParseError {
            command: format_command(&self.bv_bin, &["--robot-plan"]),
            source,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn insights(&self) -> Result<BvInsightsOutput, BvError> {
        let output = self.run(&["--robot-insights"])?;
        parse_insights_output(&output.stdout).map_err(|source| BvError::ParseError {
            command: format_command(&self.bv_bin, &["--robot-insights"]),
            source,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn priority_audit(&self) -> Result<BvPriorityAuditOutput, BvError> {
        let output = self.run(&["--robot-priority"])?;
        parse_priority_audit_output(&output.stdout).map_err(|source| BvError::ParseError {
            command: format_command(&self.bv_bin, &["--robot-priority"]),
            source,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn alerts(&self) -> Result<BvAlertsOutput, BvError> {
        let output = self.run(&["--robot-alerts"])?;
        parse_alerts_output(&output.stdout).map_err(|source| BvError::ParseError {
            command: format_command(&self.bv_bin, &["--robot-alerts"]),
            source,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn capability(&self) -> Result<BvCapability, BvError> {
        let beads_dir_exists = self.working_dir.join(".beads").exists();
        let output = self.run(&["--version"])?;
        let version =
            first_non_empty_line(&output.stdout).or_else(|| first_non_empty_line(&output.stderr));
        Ok(BvCapability {
            available: true,
            version,
            beads_dir_exists,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BvError {
    #[error("bv binary not found at {path}")]
    NotFound { path: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("bv command failed ({command}) with exit code {code:?}")]
    CommandFailed {
        command: String,
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    #[error("failed to parse bv output for {command}: {source}")]
    ParseError {
        command: String,
        source: serde_json::Error,
        stdout: String,
        stderr: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BvCapability {
    pub available: bool,
    pub version: Option<String>,
    pub beads_dir_exists: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvTriageOutput {
    pub generated_at: Timestamp,
    pub data_hash: String,
    pub meta: BvTriageMeta,
    pub quick_ref: BvQuickRef,
    pub recommendations: Vec<BvRecommendation>,
    pub quick_wins: Vec<BvQuickWin>,
    pub blockers_to_clear: Vec<BvBlocker>,
    pub project_health: BvProjectHealth,
    pub commands: Vec<BvCommand>,
    pub usage_hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvTriageMeta {
    pub version: String,
    pub generated_at: Timestamp,
    pub phase2_ready: bool,
    pub issue_count: usize,
    pub compute_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvQuickRef {
    pub open_count: usize,
    pub actionable_count: usize,
    pub blocked_count: usize,
    pub in_progress_count: usize,
    pub top_picks: Vec<BvTopPick>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvTopPick {
    pub id: BeadId,
    pub title: String,
    pub score: f64,
    pub reasons: Vec<String>,
    pub unblocks: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvRecommendation {
    pub id: BeadId,
    pub title: String,
    pub issue_type: String,
    pub status: String,
    pub priority: BeadPriority,
    pub labels: Vec<String>,
    pub score: f64,
    pub breakdown_json: Value,
    pub action: Option<String>,
    pub reasons: Vec<String>,
    pub unblocks: Vec<BeadId>,
    pub blocked_by: Vec<BeadId>,
    pub page_rank: Option<f64>,
    pub betweenness: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvQuickWin {
    pub id: BeadId,
    pub title: String,
    pub score: f64,
    pub reason: String,
    pub unblocks: Vec<BeadId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvBlocker {
    pub id: BeadId,
    pub title: String,
    pub unblocks_count: usize,
    pub unblocks: Vec<BeadId>,
    pub actionable: bool,
    pub blocked_by: Vec<BeadId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvProjectHealth {
    pub counts: BvProjectCounts,
    pub graph: BvGraphHealth,
    pub velocity: BvVelocitySummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BvProjectCounts {
    pub total: usize,
    pub open: usize,
    pub closed: usize,
    pub blocked: usize,
    pub actionable: usize,
    pub by_status: HashMap<String, usize>,
    pub by_type: HashMap<String, usize>,
    pub by_priority: HashMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvGraphHealth {
    pub node_count: usize,
    pub edge_count: usize,
    pub density: Option<f64>,
    pub has_cycles: bool,
    pub phase2_ready: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvVelocitySummary {
    pub closed_last_7_days: usize,
    pub closed_last_30_days: usize,
    pub avg_days_to_close: Option<f64>,
    pub weekly: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BvCommand {
    pub label: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvNextOutput {
    pub generated_at: Timestamp,
    pub data_hash: String,
    pub id: BeadId,
    pub title: String,
    pub score: f64,
    pub reasons: Vec<String>,
    pub unblocks: usize,
    pub claim_command: Option<String>,
    pub show_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvPlanOutput {
    pub generated_at: Timestamp,
    pub data_hash: String,
    pub analysis_config_json: Value,
    pub status: HashMap<String, BvMetricStatus>,
    pub tracks: Vec<BvTrack>,
    pub total_actionable: usize,
    pub total_blocked: usize,
    pub summary: BvPlanSummary,
    pub usage_hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvTrack {
    pub track_id: String,
    pub items: Vec<BvTrackItem>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvTrackItem {
    pub id: BeadId,
    pub title: String,
    pub priority: BeadPriority,
    pub status: String,
    pub unblocks: Vec<BeadId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvPlanSummary {
    pub highest_impact: Option<BeadId>,
    pub impact_reason: Option<String>,
    pub unblocks_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BvMetricStatus {
    pub state: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvInsightsOutput {
    pub generated_at: Timestamp,
    pub data_hash: String,
    pub analysis_config_json: Value,
    pub status: HashMap<String, BvMetricStatus>,
    pub bottlenecks: Vec<BvScoredIssue>,
    pub keystones: Vec<BvScoredIssue>,
    pub influencers: Vec<BvScoredIssue>,
    pub hubs: Vec<BvScoredIssue>,
    pub authorities: Vec<BvScoredIssue>,
    pub cores: Vec<BvScoredIssue>,
    pub slack: Vec<BvScoredIssue>,
    pub articulation_points: Vec<BeadId>,
    pub orphans: Vec<BeadId>,
    pub cycles: Vec<Vec<BeadId>>,
    pub cluster_density: Option<f64>,
    pub velocity: BvVelocitySummary,
    pub stats: BvGraphStats,
    pub full_stats: BvFullStats,
    pub top_what_ifs: Vec<BvWhatIfImpact>,
    pub advanced_insights_json: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvScoredIssue {
    pub id: BeadId,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvGraphStats {
    pub out_degree: HashMap<BeadId, usize>,
    pub in_degree: HashMap<BeadId, usize>,
    pub topological_order: Vec<BeadId>,
    pub density: Option<f64>,
    pub node_count: usize,
    pub edge_count: usize,
    pub config_json: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvFullStats {
    pub page_rank: HashMap<BeadId, f64>,
    pub betweenness: HashMap<BeadId, f64>,
    pub eigenvector: HashMap<BeadId, f64>,
    pub hubs: HashMap<BeadId, f64>,
    pub authorities: HashMap<BeadId, f64>,
    pub critical_path_score: HashMap<BeadId, f64>,
    pub core_number: HashMap<BeadId, usize>,
    pub slack: HashMap<BeadId, f64>,
    pub articulation_points: Vec<BeadId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvWhatIfImpact {
    pub issue_id: BeadId,
    pub title: String,
    pub delta: BvWhatIfDelta,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvWhatIfDelta {
    pub direct_unblocks: usize,
    pub transitive_unblocks: usize,
    pub blocked_reduction: usize,
    pub depth_reduction: usize,
    pub estimated_days_saved: Option<f64>,
    pub unblocked_issue_ids: Vec<BeadId>,
    pub parallelization_gain: f64,
    pub explanation: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvPriorityAuditOutput {
    pub generated_at: Timestamp,
    pub data_hash: String,
    pub analysis_config_json: Value,
    pub field_descriptions_json: Value,
    pub filters_json: Value,
    pub recommendations: Vec<BvPriorityRecommendation>,
    pub status: HashMap<String, BvMetricStatus>,
    pub summary_json: Value,
    pub usage_hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvPriorityRecommendation {
    pub issue_id: BeadId,
    pub title: String,
    pub current_priority: BeadPriority,
    pub suggested_priority: BeadPriority,
    pub direction: String,
    pub confidence: f64,
    pub impact_score: f64,
    pub explanation: String,
    pub reasoning_json: Value,
    pub what_if_json: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BvAlertsOutput {
    pub generated_at: Timestamp,
    pub data_hash: String,
    pub output_format: Option<String>,
    pub alerts: Vec<Value>,
    pub summary: BvAlertSummary,
    pub usage_hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BvAlertSummary {
    pub total: usize,
    pub critical: usize,
    pub warning: usize,
    pub info: usize,
}

pub(crate) fn parse_triage_output(text: &str) -> Result<BvTriageOutput, serde_json::Error> {
    serde_json::from_str::<TriageWire>(text)?.try_into()
}

pub(crate) fn parse_next_output(text: &str) -> Result<BvNextOutput, serde_json::Error> {
    serde_json::from_str::<NextWire>(text)?.try_into()
}

pub(crate) fn parse_plan_output(text: &str) -> Result<BvPlanOutput, serde_json::Error> {
    serde_json::from_str::<PlanWire>(text)?.try_into()
}

pub(crate) fn parse_insights_output(text: &str) -> Result<BvInsightsOutput, serde_json::Error> {
    serde_json::from_str::<InsightsWire>(text)?.try_into()
}

pub(crate) fn parse_priority_audit_output(
    text: &str,
) -> Result<BvPriorityAuditOutput, serde_json::Error> {
    serde_json::from_str::<PriorityAuditWire>(text)?.try_into()
}

pub(crate) fn parse_alerts_output(text: &str) -> Result<BvAlertsOutput, serde_json::Error> {
    serde_json::from_str::<AlertsWire>(text)?.try_into()
}

#[derive(Debug, Deserialize)]
struct TriageWire {
    generated_at: String,
    data_hash: String,
    triage: TriagePayloadWire,
    #[serde(default)]
    usage_hints: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TriagePayloadWire {
    meta: TriageMetaWire,
    quick_ref: QuickRefWire,
    #[serde(default)]
    recommendations: Vec<RecommendationWire>,
    #[serde(default)]
    quick_wins: Vec<QuickWinWire>,
    #[serde(default)]
    blockers_to_clear: Vec<BlockerWire>,
    project_health: ProjectHealthWire,
    #[serde(default)]
    commands: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct TriageMetaWire {
    version: String,
    generated_at: String,
    phase2_ready: bool,
    issue_count: usize,
    compute_time_ms: u64,
}

#[derive(Debug, Deserialize)]
struct QuickRefWire {
    open_count: usize,
    actionable_count: usize,
    blocked_count: usize,
    in_progress_count: usize,
    #[serde(default)]
    top_picks: Vec<TopPickWire>,
}

#[derive(Debug, Deserialize)]
struct TopPickWire {
    id: String,
    title: String,
    score: f64,
    #[serde(default)]
    reasons: Vec<String>,
    #[serde(default)]
    unblocks: usize,
}

#[derive(Debug, Deserialize)]
struct RecommendationWire {
    id: String,
    title: String,
    #[serde(rename = "type")]
    issue_type: String,
    status: String,
    priority: i32,
    #[serde(default)]
    labels: Vec<String>,
    score: f64,
    #[serde(default)]
    breakdown: Value,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    reasons: Vec<String>,
    #[serde(default)]
    unblocks_ids: Vec<String>,
    #[serde(default)]
    blocked_by: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct QuickWinWire {
    id: String,
    title: String,
    score: f64,
    reason: String,
    #[serde(default)]
    unblocks_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BlockerWire {
    id: String,
    title: String,
    unblocks_count: usize,
    #[serde(default)]
    unblocks_ids: Vec<String>,
    #[serde(default)]
    actionable: bool,
    #[serde(default)]
    blocked_by: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectHealthWire {
    counts: ProjectCountsWire,
    graph: GraphHealthWire,
    velocity: VelocityWire,
}

#[derive(Debug, Deserialize)]
struct ProjectCountsWire {
    total: usize,
    open: usize,
    closed: usize,
    blocked: usize,
    actionable: usize,
    #[serde(default)]
    by_status: HashMap<String, usize>,
    #[serde(default)]
    by_type: HashMap<String, usize>,
    #[serde(default)]
    by_priority: HashMap<String, usize>,
}

#[derive(Debug, Deserialize)]
struct GraphHealthWire {
    node_count: usize,
    edge_count: usize,
    #[serde(default)]
    density: Option<f64>,
    has_cycles: bool,
    phase2_ready: bool,
}

#[derive(Debug, Deserialize)]
struct VelocityWire {
    #[serde(default)]
    closed_last_7_days: usize,
    #[serde(default)]
    closed_last_30_days: usize,
    #[serde(default)]
    avg_days_to_close: Option<f64>,
    #[serde(default)]
    weekly: Vec<WeeklyVelocityEntryWire>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WeeklyVelocityEntryWire {
    Count(usize),
    Bucket(WeeklyVelocityBucketWire),
}

#[derive(Debug, Deserialize)]
struct WeeklyVelocityBucketWire {
    closed: usize,
}

#[derive(Debug, Deserialize)]
struct NextWire {
    generated_at: String,
    data_hash: String,
    id: String,
    title: String,
    score: f64,
    #[serde(default)]
    reasons: Vec<String>,
    #[serde(default)]
    unblocks: usize,
    #[serde(default)]
    claim_command: Option<String>,
    #[serde(default)]
    show_command: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlanWire {
    generated_at: String,
    data_hash: String,
    #[serde(default)]
    analysis_config: Value,
    #[serde(default)]
    status: HashMap<String, MetricStatusWire>,
    plan: PlanPayloadWire,
    #[serde(default)]
    usage_hints: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PlanPayloadWire {
    #[serde(default)]
    tracks: Vec<TrackWire>,
    total_actionable: usize,
    total_blocked: usize,
    summary: PlanSummaryWire,
}

#[derive(Debug, Deserialize)]
struct TrackWire {
    track_id: String,
    #[serde(default)]
    items: Vec<TrackItemWire>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TrackItemWire {
    id: String,
    title: String,
    priority: i32,
    status: String,
    #[serde(default)]
    unblocks: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct PlanSummaryWire {
    #[serde(default)]
    highest_impact: Option<String>,
    #[serde(default)]
    impact_reason: Option<String>,
    #[serde(default)]
    unblocks_count: usize,
}

#[derive(Debug, Deserialize)]
struct MetricStatusWire {
    state: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InsightsWire {
    generated_at: String,
    data_hash: String,
    #[serde(default)]
    analysis_config: Value,
    #[serde(default)]
    status: HashMap<String, MetricStatusWire>,
    #[serde(rename = "Bottlenecks", default)]
    bottlenecks: Vec<ScoredIssueWire>,
    #[serde(rename = "Keystones", default)]
    keystones: Vec<ScoredIssueWire>,
    #[serde(rename = "Influencers", default)]
    influencers: Vec<ScoredIssueWire>,
    #[serde(rename = "Hubs", default)]
    hubs: Vec<ScoredIssueWire>,
    #[serde(rename = "Authorities", default)]
    authorities: Vec<ScoredIssueWire>,
    #[serde(rename = "Cores", default)]
    cores: Vec<ScoredIssueWire>,
    #[serde(rename = "Articulation", default)]
    articulation: Option<Vec<String>>,
    #[serde(rename = "Slack", default)]
    slack: Vec<ScoredIssueWire>,
    #[serde(rename = "Orphans", default)]
    orphans: Vec<String>,
    #[serde(rename = "Cycles", default)]
    cycles: Option<Vec<Vec<String>>>,
    #[serde(rename = "ClusterDensity", default)]
    cluster_density: Option<f64>,
    #[serde(rename = "Velocity")]
    velocity: VelocityWire,
    #[serde(rename = "Stats")]
    stats: GraphStatsWire,
    full_stats: FullStatsWire,
    #[serde(default)]
    top_what_ifs: Vec<WhatIfWire>,
    #[serde(default)]
    advanced_insights: Value,
}

#[derive(Debug, Deserialize)]
struct ScoredIssueWire {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Value")]
    value: f64,
}

#[derive(Debug, Deserialize)]
struct GraphStatsWire {
    #[serde(rename = "OutDegree", default)]
    out_degree: HashMap<String, usize>,
    #[serde(rename = "InDegree", default)]
    in_degree: HashMap<String, usize>,
    #[serde(rename = "TopologicalOrder", default)]
    topological_order: Vec<String>,
    #[serde(rename = "Density", default)]
    density: Option<f64>,
    #[serde(rename = "NodeCount")]
    node_count: usize,
    #[serde(rename = "EdgeCount")]
    edge_count: usize,
    #[serde(rename = "Config", default)]
    config: Value,
}

#[derive(Debug, Deserialize)]
struct FullStatsWire {
    #[serde(default)]
    pagerank: HashMap<String, f64>,
    #[serde(default)]
    betweenness: HashMap<String, f64>,
    #[serde(default)]
    eigenvector: HashMap<String, f64>,
    #[serde(default)]
    hubs: HashMap<String, f64>,
    #[serde(default)]
    authorities: HashMap<String, f64>,
    #[serde(default)]
    critical_path_score: HashMap<String, f64>,
    #[serde(default)]
    core_number: HashMap<String, usize>,
    #[serde(default)]
    slack: HashMap<String, f64>,
    #[serde(default)]
    articulation_points: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WhatIfWire {
    issue_id: String,
    title: String,
    delta: WhatIfDeltaWire,
}

#[derive(Debug, Deserialize)]
struct WhatIfDeltaWire {
    direct_unblocks: usize,
    transitive_unblocks: usize,
    blocked_reduction: usize,
    depth_reduction: usize,
    #[serde(default)]
    estimated_days_saved: Option<f64>,
    #[serde(default)]
    unblocked_issue_ids: Vec<String>,
    #[serde(default)]
    parallelization_gain: f64,
    #[serde(default)]
    explanation: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PriorityAuditWire {
    generated_at: String,
    data_hash: String,
    #[serde(default)]
    analysis_config: Value,
    #[serde(default)]
    field_descriptions: Value,
    #[serde(default)]
    filters: Value,
    #[serde(default)]
    recommendations: Vec<PriorityRecommendationWire>,
    #[serde(default)]
    status: HashMap<String, MetricStatusWire>,
    #[serde(default)]
    summary: Value,
    #[serde(default)]
    usage_hints: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PriorityRecommendationWire {
    issue_id: String,
    title: String,
    current_priority: i32,
    suggested_priority: i32,
    direction: String,
    confidence: f64,
    impact_score: f64,
    explanation: String,
    #[serde(default)]
    reasoning: Value,
    #[serde(default)]
    what_if: Value,
}

#[derive(Debug, Deserialize)]
struct AlertsWire {
    generated_at: String,
    data_hash: String,
    #[serde(default)]
    output_format: Option<String>,
    #[serde(default)]
    alerts: Vec<Value>,
    summary: AlertSummaryWire,
    #[serde(default)]
    usage_hints: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AlertSummaryWire {
    total: usize,
    critical: usize,
    warning: usize,
    info: usize,
}

impl TryFrom<TriageWire> for BvTriageOutput {
    type Error = serde_json::Error;

    fn try_from(value: TriageWire) -> Result<Self, Self::Error> {
        let mut commands: Vec<BvCommand> = value
            .triage
            .commands
            .into_iter()
            .map(|(label, command)| BvCommand { label, command })
            .collect();
        commands.sort_by(|left, right| left.label.cmp(&right.label));

        Ok(Self {
            generated_at: parse_timestamp(&value.generated_at)
                .map_err(serde_json::Error::custom)?,
            data_hash: value.data_hash,
            meta: value.triage.meta.try_into()?,
            quick_ref: value.triage.quick_ref.try_into()?,
            recommendations: value
                .triage
                .recommendations
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            quick_wins: value
                .triage
                .quick_wins
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            blockers_to_clear: value
                .triage
                .blockers_to_clear
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            project_health: value.triage.project_health.try_into()?,
            commands,
            usage_hints: value.usage_hints,
        })
    }
}

impl TryFrom<TriageMetaWire> for BvTriageMeta {
    type Error = serde_json::Error;

    fn try_from(value: TriageMetaWire) -> Result<Self, Self::Error> {
        Ok(Self {
            version: value.version,
            generated_at: parse_timestamp(&value.generated_at)
                .map_err(serde_json::Error::custom)?,
            phase2_ready: value.phase2_ready,
            issue_count: value.issue_count,
            compute_time_ms: value.compute_time_ms,
        })
    }
}

impl TryFrom<QuickRefWire> for BvQuickRef {
    type Error = serde_json::Error;

    fn try_from(value: QuickRefWire) -> Result<Self, Self::Error> {
        Ok(Self {
            open_count: value.open_count,
            actionable_count: value.actionable_count,
            blocked_count: value.blocked_count,
            in_progress_count: value.in_progress_count,
            top_picks: value
                .top_picks
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TryFrom<TopPickWire> for BvTopPick {
    type Error = serde_json::Error;

    fn try_from(value: TopPickWire) -> Result<Self, Self::Error> {
        Ok(Self {
            id: BeadId::new(value.id),
            title: value.title,
            score: value.score,
            reasons: value.reasons,
            unblocks: value.unblocks,
        })
    }
}

impl TryFrom<RecommendationWire> for BvRecommendation {
    type Error = serde_json::Error;

    fn try_from(value: RecommendationWire) -> Result<Self, Self::Error> {
        let page_rank = value.breakdown.get("pagerank").and_then(Value::as_f64);
        let betweenness = value.breakdown.get("betweenness").and_then(Value::as_f64);

        Ok(Self {
            id: BeadId::new(value.id),
            title: value.title,
            issue_type: value.issue_type,
            status: value.status,
            priority: parse_priority(value.priority).map_err(serde_json::Error::custom)?,
            labels: value.labels,
            score: value.score,
            breakdown_json: value.breakdown,
            action: value.action,
            reasons: value.reasons,
            unblocks: bead_ids(value.unblocks_ids),
            blocked_by: bead_ids(value.blocked_by),
            page_rank,
            betweenness,
        })
    }
}

impl TryFrom<QuickWinWire> for BvQuickWin {
    type Error = serde_json::Error;

    fn try_from(value: QuickWinWire) -> Result<Self, Self::Error> {
        Ok(Self {
            id: BeadId::new(value.id),
            title: value.title,
            score: value.score,
            reason: value.reason,
            unblocks: bead_ids(value.unblocks_ids),
        })
    }
}

impl TryFrom<BlockerWire> for BvBlocker {
    type Error = serde_json::Error;

    fn try_from(value: BlockerWire) -> Result<Self, Self::Error> {
        Ok(Self {
            id: BeadId::new(value.id),
            title: value.title,
            unblocks_count: value.unblocks_count,
            unblocks: bead_ids(value.unblocks_ids),
            actionable: value.actionable,
            blocked_by: bead_ids(value.blocked_by),
        })
    }
}

impl TryFrom<ProjectHealthWire> for BvProjectHealth {
    type Error = serde_json::Error;

    fn try_from(value: ProjectHealthWire) -> Result<Self, Self::Error> {
        Ok(Self {
            counts: BvProjectCounts {
                total: value.counts.total,
                open: value.counts.open,
                closed: value.counts.closed,
                blocked: value.counts.blocked,
                actionable: value.counts.actionable,
                by_status: value.counts.by_status,
                by_type: value.counts.by_type,
                by_priority: value.counts.by_priority,
            },
            graph: BvGraphHealth {
                node_count: value.graph.node_count,
                edge_count: value.graph.edge_count,
                density: value.graph.density,
                has_cycles: value.graph.has_cycles,
                phase2_ready: value.graph.phase2_ready,
            },
            velocity: value.velocity.into(),
        })
    }
}

impl TryFrom<NextWire> for BvNextOutput {
    type Error = serde_json::Error;

    fn try_from(value: NextWire) -> Result<Self, Self::Error> {
        Ok(Self {
            generated_at: parse_timestamp(&value.generated_at)
                .map_err(serde_json::Error::custom)?,
            data_hash: value.data_hash,
            id: BeadId::new(value.id),
            title: value.title,
            score: value.score,
            reasons: value.reasons,
            unblocks: value.unblocks,
            claim_command: value.claim_command,
            show_command: value.show_command,
        })
    }
}

impl TryFrom<PlanWire> for BvPlanOutput {
    type Error = serde_json::Error;

    fn try_from(value: PlanWire) -> Result<Self, Self::Error> {
        Ok(Self {
            generated_at: parse_timestamp(&value.generated_at)
                .map_err(serde_json::Error::custom)?,
            data_hash: value.data_hash,
            analysis_config_json: value.analysis_config,
            status: metric_status_map(value.status),
            tracks: value
                .plan
                .tracks
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            total_actionable: value.plan.total_actionable,
            total_blocked: value.plan.total_blocked,
            summary: value.plan.summary.try_into()?,
            usage_hints: value.usage_hints,
        })
    }
}

impl TryFrom<TrackWire> for BvTrack {
    type Error = serde_json::Error;

    fn try_from(value: TrackWire) -> Result<Self, Self::Error> {
        Ok(Self {
            track_id: value.track_id,
            items: value
                .items
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            reason: value.reason,
        })
    }
}

impl TryFrom<TrackItemWire> for BvTrackItem {
    type Error = serde_json::Error;

    fn try_from(value: TrackItemWire) -> Result<Self, Self::Error> {
        Ok(Self {
            id: BeadId::new(value.id),
            title: value.title,
            priority: parse_priority(value.priority).map_err(serde_json::Error::custom)?,
            status: value.status,
            unblocks: bead_ids(value.unblocks.unwrap_or_default()),
        })
    }
}

impl TryFrom<PlanSummaryWire> for BvPlanSummary {
    type Error = serde_json::Error;

    fn try_from(value: PlanSummaryWire) -> Result<Self, Self::Error> {
        Ok(Self {
            highest_impact: value.highest_impact.map(BeadId::new),
            impact_reason: value.impact_reason,
            unblocks_count: value.unblocks_count,
        })
    }
}

impl From<MetricStatusWire> for BvMetricStatus {
    fn from(value: MetricStatusWire) -> Self {
        Self {
            state: value.state,
            reason: value.reason,
        }
    }
}

impl TryFrom<InsightsWire> for BvInsightsOutput {
    type Error = serde_json::Error;

    fn try_from(value: InsightsWire) -> Result<Self, Self::Error> {
        let full_stats: BvFullStats = value.full_stats.try_into()?;
        let articulation_points = value
            .articulation
            .map(bead_ids)
            .unwrap_or_else(|| full_stats.articulation_points.clone());

        Ok(Self {
            generated_at: parse_timestamp(&value.generated_at)
                .map_err(serde_json::Error::custom)?,
            data_hash: value.data_hash,
            analysis_config_json: value.analysis_config,
            status: metric_status_map(value.status),
            bottlenecks: scored_issues(value.bottlenecks),
            keystones: scored_issues(value.keystones),
            influencers: scored_issues(value.influencers),
            hubs: scored_issues(value.hubs),
            authorities: scored_issues(value.authorities),
            cores: scored_issues(value.cores),
            slack: scored_issues(value.slack),
            articulation_points,
            orphans: bead_ids(value.orphans),
            cycles: value
                .cycles
                .unwrap_or_default()
                .into_iter()
                .map(bead_ids)
                .collect(),
            cluster_density: value.cluster_density,
            velocity: value.velocity.into(),
            stats: value.stats.into(),
            full_stats,
            top_what_ifs: value
                .top_what_ifs
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            advanced_insights_json: value.advanced_insights,
        })
    }
}

impl From<VelocityWire> for BvVelocitySummary {
    fn from(value: VelocityWire) -> Self {
        Self {
            closed_last_7_days: value.closed_last_7_days,
            closed_last_30_days: value.closed_last_30_days,
            avg_days_to_close: value.avg_days_to_close,
            weekly: value
                .weekly
                .into_iter()
                .map(WeeklyVelocityEntryWire::closed_count)
                .collect(),
        }
    }
}

impl WeeklyVelocityEntryWire {
    fn closed_count(self) -> usize {
        match self {
            Self::Count(count) => count,
            Self::Bucket(bucket) => bucket.closed,
        }
    }
}

impl From<GraphStatsWire> for BvGraphStats {
    fn from(value: GraphStatsWire) -> Self {
        Self {
            out_degree: bead_id_map(value.out_degree),
            in_degree: bead_id_map(value.in_degree),
            topological_order: bead_ids(value.topological_order),
            density: value.density,
            node_count: value.node_count,
            edge_count: value.edge_count,
            config_json: value.config,
        }
    }
}

impl TryFrom<FullStatsWire> for BvFullStats {
    type Error = serde_json::Error;

    fn try_from(value: FullStatsWire) -> Result<Self, Self::Error> {
        Ok(Self {
            page_rank: bead_id_map(value.pagerank),
            betweenness: bead_id_map(value.betweenness),
            eigenvector: bead_id_map(value.eigenvector),
            hubs: bead_id_map(value.hubs),
            authorities: bead_id_map(value.authorities),
            critical_path_score: bead_id_map(value.critical_path_score),
            core_number: bead_id_map(value.core_number),
            slack: bead_id_map(value.slack),
            articulation_points: bead_ids(value.articulation_points),
        })
    }
}

impl TryFrom<WhatIfWire> for BvWhatIfImpact {
    type Error = serde_json::Error;

    fn try_from(value: WhatIfWire) -> Result<Self, Self::Error> {
        Ok(Self {
            issue_id: BeadId::new(value.issue_id),
            title: value.title,
            delta: value.delta.into(),
        })
    }
}

impl From<WhatIfDeltaWire> for BvWhatIfDelta {
    fn from(value: WhatIfDeltaWire) -> Self {
        Self {
            direct_unblocks: value.direct_unblocks,
            transitive_unblocks: value.transitive_unblocks,
            blocked_reduction: value.blocked_reduction,
            depth_reduction: value.depth_reduction,
            estimated_days_saved: value.estimated_days_saved,
            unblocked_issue_ids: bead_ids(value.unblocked_issue_ids),
            parallelization_gain: value.parallelization_gain,
            explanation: value.explanation,
        }
    }
}

impl TryFrom<PriorityAuditWire> for BvPriorityAuditOutput {
    type Error = serde_json::Error;

    fn try_from(value: PriorityAuditWire) -> Result<Self, Self::Error> {
        Ok(Self {
            generated_at: parse_timestamp(&value.generated_at)
                .map_err(serde_json::Error::custom)?,
            data_hash: value.data_hash,
            analysis_config_json: value.analysis_config,
            field_descriptions_json: value.field_descriptions,
            filters_json: value.filters,
            recommendations: value
                .recommendations
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            status: metric_status_map(value.status),
            summary_json: value.summary,
            usage_hints: value.usage_hints,
        })
    }
}

impl TryFrom<PriorityRecommendationWire> for BvPriorityRecommendation {
    type Error = serde_json::Error;

    fn try_from(value: PriorityRecommendationWire) -> Result<Self, Self::Error> {
        Ok(Self {
            issue_id: BeadId::new(value.issue_id),
            title: value.title,
            current_priority: parse_priority(value.current_priority)
                .map_err(serde_json::Error::custom)?,
            suggested_priority: parse_priority(value.suggested_priority)
                .map_err(serde_json::Error::custom)?,
            direction: value.direction,
            confidence: value.confidence,
            impact_score: value.impact_score,
            explanation: value.explanation,
            reasoning_json: value.reasoning,
            what_if_json: value.what_if,
        })
    }
}

impl TryFrom<AlertsWire> for BvAlertsOutput {
    type Error = serde_json::Error;

    fn try_from(value: AlertsWire) -> Result<Self, Self::Error> {
        Ok(Self {
            generated_at: parse_timestamp(&value.generated_at)
                .map_err(serde_json::Error::custom)?,
            data_hash: value.data_hash,
            output_format: value.output_format,
            alerts: value.alerts,
            summary: value.summary.into(),
            usage_hints: value.usage_hints,
        })
    }
}

impl From<AlertSummaryWire> for BvAlertSummary {
    fn from(value: AlertSummaryWire) -> Self {
        Self {
            total: value.total,
            critical: value.critical,
            warning: value.warning,
            info: value.info,
        }
    }
}

struct CommandOutput {
    stdout: String,
    stderr: String,
}

pub(crate) fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

fn metric_status_map(input: HashMap<String, MetricStatusWire>) -> HashMap<String, BvMetricStatus> {
    input
        .into_iter()
        .map(|(name, status)| (name, status.into()))
        .collect()
}

fn scored_issues(input: Vec<ScoredIssueWire>) -> Vec<BvScoredIssue> {
    input
        .into_iter()
        .map(|item| BvScoredIssue {
            id: BeadId::new(item.id),
            value: item.value,
        })
        .collect()
}

fn bead_ids(values: Vec<String>) -> Vec<BeadId> {
    values.into_iter().map(BeadId::new).collect()
}

fn bead_id_map<T>(input: HashMap<String, T>) -> HashMap<BeadId, T> {
    input
        .into_iter()
        .map(|(id, value)| (BeadId::new(id), value))
        .collect()
}

fn parse_timestamp(input: &str) -> Result<Timestamp, String> {
    chrono::DateTime::parse_from_rfc3339(input)
        .map(|ts| ts.with_timezone(&chrono::Utc))
        .map_err(|err| format!("invalid timestamp `{input}`: {err}"))
}

fn parse_priority(value: i32) -> Result<BeadPriority, String> {
    match value {
        0 => Ok(BeadPriority::P0),
        1 => Ok(BeadPriority::P1),
        2 => Ok(BeadPriority::P2),
        3 => Ok(BeadPriority::P3),
        4 => Ok(BeadPriority::P4),
        other => Err(format!("unsupported bead priority `{other}`")),
    }
}

fn format_command(binary: &str, args: &[&str]) -> String {
    let joined = args.join(" ");
    if joined.is_empty() {
        binary.to_owned()
    } else {
        format!("{binary} {joined}")
    }
}

impl fmt::Display for CliBvClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} @ {}", self.bv_bin, self.working_dir.display())
    }
}

#[cfg(test)]
mod tests;
