use clap::Subcommand;

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Show full daemon config
    Get,
    /// Show agent config for a project
    Agent { project: String },
}

pub async fn handle(client: &DaemonAPIClient, cmd: &ConfigCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        ConfigCommand::Get => {
            let config = client.get_config().await?;
            output::print_output(json, &config);
        }
        ConfigCommand::Agent { project } => {
            let cfg = client.get_agent_config(project).await?;
            output::print_output(json, &cfg);
        }
    }
    Ok(())
}
