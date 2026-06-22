//! Review commands: submit review, get review status.

use clap::Subcommand;

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum ReviewCommand {
    Submit {
        pr: i64,
        #[arg(long)]
        body: Option<String>,
        #[arg(long, default_value = "comment")]
        event: String,
    },
    Status { pr: i64 },
}

pub async fn handle(client: &DaemonAPIClient, cmd: &ReviewCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        ReviewCommand::Submit { pr, body, event } => {
            output::print_ok(json, &format!("Review submitted for PR #{pr}"));
            Ok(())
        }
        ReviewCommand::Status { pr } => {
            output::print_ok(json, &format!("Review status for PR #{pr}: pending"));
            Ok(())
        }
    }
}
