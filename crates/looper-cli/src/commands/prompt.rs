//! Prompt inspection commands.

use crate::client::DaemonAPIClient;
use crate::error::CliError;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum PromptCommand {
    /// Show agent prompts per role
    Preview,
}

pub async fn handle(_client: &DaemonAPIClient, cmd: &PromptCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        PromptCommand::Preview => preview_prompts(json).await,
    }
}

async fn preview_prompts(json: bool) -> Result<(), CliError> {
    let roles = vec![
        ("planner", "Reads issues, explores repo, drafts spec, opens spec PR"),
        ("reviewer", "Reviews PR, posts inline threads, auto-merges when ready"),
        ("worker", "Implements spec/issue, creates PR, runs checks"),
        ("fixer", "Reads review comments, fixes in worktree, pushes, resolves threads"),
        ("coordinator", "Triages fresh issues, dispatches to planner/worker"),
    ];

    if json {
        let items: Vec<serde_json::Value> =
            roles.iter().map(|(role, desc)| serde_json::json!({"role": role, "description": desc})).collect();
        println!("{}", serde_json::to_string_pretty(&items).unwrap_or_default());
    } else {
        println!("Agent Roles and Prompts");
        println!("{}", "-".repeat(60));
        for (role, desc) in &roles {
            println!("\n  {}:", role.to_uppercase());
            println!("    {}", desc);
        }
        println!();
        println!("Prompts are configured per-role in looper.toml [roles.<role>].");
    }
    Ok(())
}
