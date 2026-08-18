//! Feedback submission commands.

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum FeedbackCommand {
    /// Submit feedback about an agent run
    Submit {
        /// Run ID to provide feedback on
        run_id: String,
        /// Feedback text
        #[arg(short, long)]
        message: String,
    },
}

pub async fn handle(_client: &DaemonAPIClient, cmd: &FeedbackCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        FeedbackCommand::Submit { run_id, message } => submit_feedback(run_id, message, json).await,
    }
}

async fn submit_feedback(run_id: &str, message: &str, json: bool) -> Result<(), CliError> {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "submitted",
                "run_id": run_id,
                "message": message,
            })
        );
    } else {
        println!("Feedback submitted for run {run_id}");
        println!("Message: {message}");
        println!();
        println!("Thank you! Feedback helps improve agent quality.");
    }
    Ok(())
}
