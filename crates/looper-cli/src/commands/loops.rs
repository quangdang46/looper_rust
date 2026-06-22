use clap::{Args, Subcommand};

use crate::client::{CreateLoopInput, DaemonAPIClient};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum LoopCommand {
    /// List loops for a project
    List { project: String },
    /// Create a new loop
    Create(CreateLoopArgs),
    /// Get loop details
    Get { project: String, seq: i64 },
    /// Pause a loop
    Pause { project: String, seq: i64 },
    /// Resume a loop
    Resume { project: String, seq: i64 },
    /// Terminate a loop
    Terminate { project: String, seq: i64 },
}

#[derive(Debug, Args)]
pub struct CreateLoopArgs {
    pub project: String,
    #[arg(long)]
    pub r#type: String,
    #[arg(long)]
    pub target: Option<String>,
    #[arg(long)]
    pub metadata: Option<String>,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &LoopCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        LoopCommand::List { project } => {
            let loops = client.list_loops(project, 0, 100).await?;
            output::print_output_vec(json, &loops);
        }
        LoopCommand::Create(args) => {
            let metadata = args.metadata.as_ref()
                .map(|s| serde_json::from_str(s))
                .transpose()
                .map_err(|e| CliError::config(format!("invalid JSON metadata: {e}")))?;
            let input = CreateLoopInput {
                loop_type: args.r#type.clone(),
                target: args.target.clone(),
                metadata,
            };
            let detail = client.create_loop(&args.project, &input).await?;
            output::print_output(json, &detail);
        }
        LoopCommand::Get { project, seq } => {
            let detail = client.get_loop(project, *seq).await?;
            output::print_output(json, &detail);
        }
        LoopCommand::Pause { project, seq } => {
            client.pause_loop(project, *seq).await?;
            output::print_ok(json, format!("Loop {project}/{seq} paused"));
        }
        LoopCommand::Resume { project, seq } => {
            client.resume_loop(project, *seq).await?;
            output::print_ok(json, format!("Loop {project}/{seq} resumed"));
        }
        LoopCommand::Terminate { project, seq } => {
            client.terminate_loop(project, *seq).await?;
            output::print_ok(json, format!("Loop {project}/{seq} terminated"));
        }
    }
    Ok(())
}
