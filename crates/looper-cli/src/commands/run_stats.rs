//! Run statistics and timeline commands.

use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum RunStatsCommand {
    Show { run_id: String },
    Timeline { run_id: String },
    History { limit: Option<usize> },
}

pub async fn handle(_client: &DaemonAPIClient, cmd: &RunStatsCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        RunStatsCommand::Show { run_id } => {
            output::print_ok(json, &format!("Stats for run {run_id}"));
            Ok(())
        }
        RunStatsCommand::Timeline { run_id } => {
            output::print_ok(json, &format!("Timeline for run {run_id}"));
            Ok(())
        }
        RunStatsCommand::History { limit } => {
            let n = limit.unwrap_or(10);
            output::print_ok(json, &format!("Last {n} runs"));
            Ok(())
        }
    }
}
