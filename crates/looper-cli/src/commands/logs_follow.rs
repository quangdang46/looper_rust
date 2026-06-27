use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum LogsFollowCommand {
    Status,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &LogsFollowCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        LogsFollowCommand::Status => {
            let s = client.health().await?;
            output::print_output(json, &s);
        }
    }
    Ok(())
}
