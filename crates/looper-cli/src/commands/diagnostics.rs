//! `looper diagnostics` — real multi-field daemon status for operators/agents.

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;
use serde::Serialize;

#[derive(Debug, Subcommand)]
pub enum DiagnosticsCommand {
    /// Show daemon health, version, projects, and queue depths
    Status,
}

#[derive(Debug, Serialize)]
struct DiagnosticsReport {
    health: crate::client::HealthResponse,
    version: String,
    project_count: usize,
    projects: Vec<ProjectDiag>,
    queue_totals: QueueTotals,
}

#[derive(Debug, Serialize)]
struct ProjectDiag {
    name: String,
    enabled: bool,
    repo_url: Option<String>,
    queue: QueueTotals,
}

#[derive(Debug, Default, Serialize)]
struct QueueTotals {
    queued: usize,
    running: usize,
    failed: usize,
    cancelled: usize,
    other: usize,
    total: usize,
}

impl QueueTotals {
    fn from_statuses(items: &[crate::client::QueueItemResponse]) -> Self {
        let mut t = Self::default();
        for i in items {
            t.total += 1;
            match i.status.as_str() {
                "queued" => t.queued += 1,
                "running" => t.running += 1,
                "failed" => t.failed += 1,
                "cancelled" => t.cancelled += 1,
                _ => t.other += 1,
            }
        }
        t
    }

    fn merge(&mut self, other: &QueueTotals) {
        self.queued += other.queued;
        self.running += other.running;
        self.failed += other.failed;
        self.cancelled += other.cancelled;
        self.other += other.other;
        self.total += other.total;
    }
}

pub async fn handle(client: &DaemonAPIClient, cmd: &DiagnosticsCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        DiagnosticsCommand::Status => {
            let health = client.health().await?;
            let version = client.server_version().await.map(|v| v.version).unwrap_or_else(|_| health.version.clone());
            let projects = client.list_projects().await?;

            let mut queue_totals = QueueTotals::default();
            let mut project_diags = Vec::with_capacity(projects.len());
            for p in &projects {
                let items = client.list_queue(&p.name, 0, 500).await.unwrap_or_default();
                let q = QueueTotals::from_statuses(&items);
                queue_totals.merge(&q);
                project_diags.push(ProjectDiag {
                    name: p.name.clone(),
                    enabled: p.enabled,
                    repo_url: p.repo_url.clone(),
                    queue: q,
                });
            }

            let report = DiagnosticsReport {
                health,
                version,
                project_count: projects.len(),
                projects: project_diags,
                queue_totals,
            };

            if json {
                println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
            } else {
                println!("status:      {}", report.health.status);
                println!("version:     {}", report.version);
                println!("uptime_s:    {}", report.health.uptime_seconds);
                println!("projects:    {}", report.project_count);
                println!(
                    "queue:       queued={} running={} failed={} cancelled={} total={}",
                    report.queue_totals.queued,
                    report.queue_totals.running,
                    report.queue_totals.failed,
                    report.queue_totals.cancelled,
                    report.queue_totals.total
                );
                for p in &report.projects {
                    println!(
                        "  - {} enabled={} repo={} q={}/{}",
                        p.name,
                        p.enabled,
                        p.repo_url.as_deref().unwrap_or("-"),
                        p.queue.queued,
                        p.queue.total
                    );
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_totals_from_statuses() {
        let items = vec![
            crate::client::QueueItemResponse {
                id: "1".into(),
                project_name: "p".into(),
                queue_type: "planner".into(),
                loop_seq: None,
                run_id: None,
                status: "queued".into(),
                priority: 1,
                attempts: 0,
                last_error: None,
                scheduled_not_before: None,
                claimed_by: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
            crate::client::QueueItemResponse {
                id: "2".into(),
                project_name: "p".into(),
                queue_type: "fixer".into(),
                loop_seq: None,
                run_id: None,
                status: "running".into(),
                priority: 1,
                attempts: 0,
                last_error: None,
                scheduled_not_before: None,
                claimed_by: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
        ];
        let t = QueueTotals::from_statuses(&items);
        assert_eq!(t.queued, 1);
        assert_eq!(t.running, 1);
        assert_eq!(t.total, 2);
    }
}
