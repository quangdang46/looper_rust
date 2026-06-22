use clap::Subcommand;
use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum LabelsCommand {
    Status,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &LabelsCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        LabelsCommand::Status => {
            let s = client.health().await?;
            output::print_output(json, &s);
        }
    }
    Ok(())
}
