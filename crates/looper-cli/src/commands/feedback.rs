use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum FeedbackCommand {
    Status,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &FeedbackCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        FeedbackCommand::Status => {
            let s = client.health().await?;
            output::print_output(json, &s);
        }
    }
    Ok(())
}
