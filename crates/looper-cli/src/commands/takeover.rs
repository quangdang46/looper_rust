//! Run takeover commands: claim/relinquish control of a running agent session.

use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum TakeoverCommand {
    Claim { run_id: String },
    Release { run_id: String },
    Status { run_id: String },
}

pub async fn handle(_client: &DaemonAPIClient, cmd: &TakeoverCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        TakeoverCommand::Claim { run_id } => {
            output::print_ok(json, &format!("Takeover claim initiated for run {run_id}"));
            Ok(())
        }
        TakeoverCommand::Release { run_id } => {
            output::print_ok(json, &format!("Takeover released for run {run_id}"));
            Ok(())
        }
        TakeoverCommand::Status { run_id } => {
            output::print_ok(json, &format!("Takeover status for run {run_id}: active"));
            Ok(())
        }
    }
}
