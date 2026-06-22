use clap::{Args, Subcommand};

use crate::client::{AcquireLockInput, DaemonAPIClient};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum LockCommand {
    /// List all locks
    List,
    /// Acquire a lock
    Acquire(AcquireLockArgs),
    /// Release a lock
    Release { resource: String },
}

#[derive(Debug, Args)]
pub struct AcquireLockArgs {
    pub resource: String,
    #[arg(long)]
    pub ttl: Option<u64>,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &LockCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        LockCommand::List => {
            let locks = client.list_locks().await?;
            output::print_output_vec(json, &locks);
        }
        LockCommand::Acquire(args) => {
            let input = AcquireLockInput {
                resource: args.resource.clone(),
                ttl_secs: args.ttl,
            };
            let lock = client.acquire_lock(&input).await?;
            output::print_output(json, &lock);
        }
        LockCommand::Release { resource } => {
            client.release_lock(resource).await?;
            output::print_ok(json, format!("Lock '{resource}' released"));
        }
    }
    Ok(())
}
