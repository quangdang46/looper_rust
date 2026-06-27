//! `looper stop <seq>` — stop a running loop by its sequence number.

use crate::client::DaemonAPIClient;
use crate::commands;
use crate::error::CliError;
use crate::output;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum StopCommand {
    /// Stop a loop by sequence number
    Stop(StopArgs),
}

#[derive(Debug, Args)]
pub struct StopArgs {
    /// Loop sequence number
    pub seq: i64,
    /// Project name (optional, auto-detected if only one project)
    #[arg(short, long)]
    pub project: Option<String>,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &StopCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        StopCommand::Stop(args) => {
            let project = commands::resolve_project(client, &args.project).await?;
            client.terminate_loop(&project, args.seq).await?;
            output::print_ok(json, &format!("Loop #{} (project={}) terminated", args.seq, project));
            Ok(())
        }
    }
}
