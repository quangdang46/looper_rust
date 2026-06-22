use clap::Subcommand;

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum EventCommand {
    /// List events for a project
    List { project: String },
}

pub async fn handle(client: &DaemonAPIClient, cmd: &EventCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        EventCommand::List { project } => {
            let events = client.list_events(project, 0, 100).await?;
            output::print_output_vec(json, &events);
        }
    }
    Ok(())
}
