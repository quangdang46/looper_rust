use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum PromptCommand {
    Status,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &PromptCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        PromptCommand::Status => {
            let s = client.health().await?;
            output::print_output(json, &s);
        }
    }
    Ok(())
}
