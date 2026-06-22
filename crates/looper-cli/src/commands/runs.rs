use clap::{Args, Subcommand};

use crate::client::{DaemonAPIClient, StartRunInput};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum RunCommand {
    /// List runs for a loop
    List { project: String, seq: i64 },
    /// Start a new run
    Start(StartRunArgs),
    /// Get run details
    Get { project: String, seq: i64, run_id: String },
    /// Cancel the current run for a loop
    Cancel { project: String, seq: i64 },
}

#[derive(Debug, Args)]
pub struct StartRunArgs {
    pub project: String,
    pub seq: i64,
    pub run_id: String,
    #[arg(long)]
    pub step: String,
    #[arg(long)]
    pub vendor: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &RunCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        RunCommand::List { project, seq } => {
            let runs = client.list_runs(project, *seq, 0, 100).await?;
            output::print_output_vec(json, &runs);
        }
        RunCommand::Start(args) => {
            let input = StartRunInput {
                run_id: args.run_id.clone(),
                step_name: args.step.clone(),
                agent_vendor: args.vendor.clone(),
                model: args.model.clone(),
            };
            let detail = client.start_run(&args.project, args.seq, &input).await?;
            output::print_output(json, &detail);
        }
        RunCommand::Get { project, seq, run_id } => {
            let detail = client.get_run(project, *seq, run_id).await?;
            output::print_output(json, &detail);
        }
        RunCommand::Cancel { project, seq } => {
            client.cancel_run(project, *seq).await?;
            output::print_ok(json, format!("Run for loop {project}/{seq} cancelled"));
        }
    }
    Ok(())
}
