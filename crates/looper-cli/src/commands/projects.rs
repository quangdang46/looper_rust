use clap::{Args, Subcommand};

use crate::client::{AddProjectInput, DaemonAPIClient};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// List all projects
    List,
    /// Add a new project
    Add(AddProjectArgs),
    /// Get project details
    Get { name: String },
    /// Remove a project
    Remove { name: String },
    /// Sync a project (discover worktrees / PRs)
    Sync { name: String },
}

#[derive(Debug, Args)]
pub struct AddProjectArgs {
    pub name: String,
    #[arg(long)]
    pub path: Option<String>,
    #[arg(long)]
    pub schedule: Option<String>,
    #[arg(long)]
    pub enabled: Option<bool>,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &ProjectCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        ProjectCommand::List => {
            let projects = client.list_projects().await?;
            output::print_output_vec(json, &projects);
        }
        ProjectCommand::Add(args) => {
            let input = AddProjectInput {
                name: args.name.clone(),
                path: args.path.clone(),
                schedule: args.schedule.clone(),
                enabled: args.enabled,
            };
            let project = client.add_project(&input).await?;
            output::print_output(json, &project);
        }
        ProjectCommand::Get { name } => {
            let project = client.get_project(name).await?;
            output::print_output(json, &project);
        }
        ProjectCommand::Remove { name } => {
            client.remove_project(name).await?;
            output::print_ok(json, format!("Project '{name}' removed"));
        }
        ProjectCommand::Sync { name } => {
            let project = client.sync_project(name).await?;
            output::print_output(json, &project);
        }
    }
    Ok(())
}
