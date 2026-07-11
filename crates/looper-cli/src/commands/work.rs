//! `looper work start` — admit role work into the daemon scheduler.

use clap::{Args, Subcommand};

use crate::client::{AdmitWorkInput, DaemonAPIClient};
use crate::error::CliError;

#[derive(Debug, Subcommand)]
pub enum WorkCommand {
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

pub async fn handle(client: &DaemonAPIClient, cmd: &WorkCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        WorkCommand::Start(args) => {
            if args.issue.is_none() && args.pr.is_none() {
                return Err(CliError::config("provide --issue and/or --pr depending on role"));
            }
            let input = AdmitWorkInput {
                role: args.role.clone(),
                issue_number: args.issue,
                pr_number: args.pr,
                repo: args.repo.clone(),
                priority: args.priority,
            };
            let result = client.admit_work(&args.project, &input).await.map_err(|e| {
                // Surface actionable repo errors cleanly.
                CliError::daemon_lifecycle(e.to_string())
            })?;

            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            } else {
                println!(
                    "admitted {} work for project {} — loop seq={} queue={} status={} tick={}",
                    result.loop_detail.loop_type,
                    args.project,
                    result.loop_detail.seq,
                    result.queue_item.id,
                    result.queue_item.status,
                    result.tick_triggered
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Wrap {
        #[command(subcommand)]
        cmd: WorkCommand,
    }

    #[test]
    fn parse_work_start_planner() {
        let w = Wrap::try_parse_from(["t", "start", "--project", "p", "--role", "planner", "--issue", "12"]).unwrap();
        match w.cmd {
            WorkCommand::Start(a) => {
                assert_eq!(a.project, "p");
                assert_eq!(a.role, "planner");
                assert_eq!(a.issue, Some(12));
            }
        }
    }
}
