use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;
use clap::Subcommand;

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
