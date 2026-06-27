use clap::{Args, Subcommand};

use crate::client::{DaemonAPIClient, EnqueueInput};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum QueueCommand {
    /// List queue items for a project
    List { project: String },
    /// Enqueue a new item
    Enqueue(EnqueueArgs),
    /// Dequeue (cancel) a specific item
    Dequeue { project: String, item_id: String },
}

#[derive(Debug, Args)]
pub struct EnqueueArgs {
    pub project: String,
    #[arg(long)]
    pub r#type: String,
    #[arg(long)]
    pub loop_seq: Option<i64>,
    #[arg(long)]
    pub priority: Option<i32>,
    #[arg(long)]
    pub payload: Option<String>,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &QueueCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        QueueCommand::List { project } => {
            let items = client.list_queue(project, 0, 100).await?;
            output::print_output_vec(json, &items);
        }
        QueueCommand::Enqueue(args) => {
            let payload = args
                .payload
                .as_ref()
                .map(|s| serde_json::from_str(s))
                .transpose()
                .map_err(|e| CliError::config(format!("invalid JSON payload: {e}")))?;
            let input = EnqueueInput {
                queue_type: args.r#type.clone(),
                loop_seq: args.loop_seq,
                priority: args.priority,
                payload,
            };
            let item = client.enqueue(&args.project, &input).await?;
            output::print_output(json, &item);
        }
        QueueCommand::Dequeue { project, item_id } => {
            client.dequeue(project, item_id).await?;
            output::print_ok(json, format!("Queue item '{item_id}' dequeued"));
        }
    }
    Ok(())
}
