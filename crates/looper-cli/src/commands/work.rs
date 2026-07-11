//! Admit role work into the daemon scheduler.
//!
//! - `looper work start --role …` — generic robot form
//! - `looper work --project … --issue N` — Go-shaped worker admit
//! - Shared helper for top-level `plan` / `review` / `fix`

use clap::{Args, Subcommand};

use crate::client::{AdmitWorkInput, DaemonAPIClient};
use crate::error::CliError;

#[derive(Debug, Args)]
pub struct WorkArgs {
    /// Nested form: `work start --role …`
    #[command(subcommand)]
    pub cmd: Option<WorkSubcommand>,

    /// Project name or id (Go-shaped: `looper work -p proj --issue N`)
    #[arg(long, short = 'p', global = true)]
    pub project: Option<String>,

    /// Issue number for worker admit when no subcommand is given
    #[arg(long, global = true)]
    pub issue: Option<i64>,

    /// PR number (optional for worker)
    #[arg(long, global = true)]
    pub pr: Option<i64>,

    /// Optional repo override `owner/name`
    #[arg(long, global = true)]
    pub repo: Option<String>,

    /// Optional queue priority
    #[arg(long, global = true)]
    pub priority: Option<i64>,
}

#[derive(Debug, Subcommand)]
pub enum WorkSubcommand {
    /// Admit planner/reviewer/worker/fixer work for a project target
    Start(WorkStartArgs),
}

#[derive(Debug, Args)]
pub struct WorkStartArgs {
    /// Project name or id
    #[arg(long, short = 'p')]
    pub project: String,

    /// Role: planner | reviewer | worker | fixer
    #[arg(long)]
    pub role: String,

    /// Issue number (planner/worker)
    #[arg(long)]
    pub issue: Option<i64>,

    /// Pull request number (reviewer/fixer/worker)
    #[arg(long)]
    pub pr: Option<i64>,

    /// Optional repo override `owner/name`
    #[arg(long)]
    pub repo: Option<String>,

    /// Optional queue priority
    #[arg(long)]
    pub priority: Option<i64>,
}

/// Shared flags for Go-shaped role admits (`plan` / `review` / `fix`).
#[derive(Debug, Args, Clone)]
pub struct RoleIssueArgs {
    /// Project name or id
    #[arg(long, short = 'p')]
    pub project: String,

    /// Issue number
    #[arg(long)]
    pub issue: Option<i64>,

    /// Pull request number
    #[arg(long)]
    pub pr: Option<i64>,

    /// Optional repo override `owner/name`
    #[arg(long)]
    pub repo: Option<String>,

    /// Optional queue priority
    #[arg(long)]
    pub priority: Option<i64>,
}

pub async fn admit_role(
    client: &DaemonAPIClient,
    project: &str,
    role: &str,
    issue: Option<i64>,
    pr: Option<i64>,
    repo: Option<String>,
    priority: Option<i64>,
    json: bool,
) -> Result<(), CliError> {
    if issue.is_none() && pr.is_none() {
        return Err(CliError::config("provide --issue and/or --pr depending on role"));
    }
    match role {
        "planner" | "worker" if issue.is_none() => {
            return Err(CliError::config(format!("{role} requires --issue")));
        }
        "reviewer" | "fixer" if pr.is_none() => {
            return Err(CliError::config(format!("{role} requires --pr")));
        }
        "planner" | "reviewer" | "worker" | "fixer" => {}
        other => {
            return Err(CliError::config(format!("unknown role '{other}' (want planner|reviewer|worker|fixer)")));
        }
    }

    let input = AdmitWorkInput { role: role.to_string(), issue_number: issue, pr_number: pr, repo, priority };
    let result = client.admit_work(project, &input).await.map_err(|e| CliError::daemon_lifecycle(e.to_string()))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
    } else {
        println!(
            "admitted {} work for project {} — loop seq={} queue={} status={} tick={}",
            result.loop_detail.loop_type,
            project,
            result.loop_detail.seq,
            result.queue_item.id,
            result.queue_item.status,
            result.tick_triggered
        );
    }
    Ok(())
}

pub async fn handle(client: &DaemonAPIClient, args: &WorkArgs, json: bool) -> Result<(), CliError> {
    match &args.cmd {
        Some(WorkSubcommand::Start(start)) => {
            admit_role(
                client,
                &start.project,
                &start.role,
                start.issue,
                start.pr,
                start.repo.clone(),
                start.priority,
                json,
            )
            .await
        }
        None => {
            // Go-shaped: looper work -p <project> --issue N
            let project = args
                .project
                .as_deref()
                .ok_or_else(|| CliError::config("provide --project (or use: work start --role …)"))?;
            admit_role(client, project, "worker", args.issue, args.pr, args.repo.clone(), args.priority, json).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Wrap {
        #[command(flatten)]
        args: WorkArgs,
    }

    #[test]
    fn parse_work_start_planner() {
        let w = Wrap::try_parse_from(["t", "start", "--project", "p", "--role", "planner", "--issue", "12"]).unwrap();
        match w.args.cmd {
            Some(WorkSubcommand::Start(a)) => {
                assert_eq!(a.project, "p");
                assert_eq!(a.role, "planner");
                assert_eq!(a.issue, Some(12));
            }
            _ => panic!("expected Start"),
        }
    }

    #[test]
    fn parse_work_go_shape() {
        let w = Wrap::try_parse_from(["t", "--project", "p", "--issue", "7"]).unwrap();
        assert!(w.args.cmd.is_none());
        assert_eq!(w.args.project.as_deref(), Some("p"));
        assert_eq!(w.args.issue, Some(7));
    }
}
