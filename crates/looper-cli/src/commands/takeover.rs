//! `looper takeover` — single-PR focus mode commands.

use crate::client::{DaemonAPIClient, StartTakeoverInput};
use crate::error::CliError;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum TakeoverCommand {
    /// Start a takeover on a specific PR
    Start(TakeoverStartArgs),
    /// List active takeover sessions
    List,
    /// Stop a takeover session
    Stop(TakeoverStopArgs),
}

#[derive(Debug, Args)]
pub struct TakeoverStartArgs {
    /// Repository in owner/repo format (e.g. "octocat/hello-world")
    #[arg(short, long)]
    pub repo: String,

    /// Pull request number to take over
    #[arg(short = 'P', long)]
    pub pr: u64,

    /// Maximum errors before giving up (default: 5)
    #[arg(long, default_value = "5")]
    pub max_errors: i32,
}

#[derive(Debug, Args)]
pub struct TakeoverStopArgs {
    /// Project name to stop takeover for (omit to stop all)
    #[arg(short, long)]
    pub project: Option<String>,
}

pub async fn handle(client: &DaemonAPIClient, cmd: &TakeoverCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        TakeoverCommand::Start(args) => start_takeover(client, args, json).await,
        TakeoverCommand::List => list_takeovers(client, json).await,
        TakeoverCommand::Stop(args) => stop_takeover(client, args, json).await,
    }
}

async fn start_takeover(client: &DaemonAPIClient, args: &TakeoverStartArgs, json: bool) -> Result<(), CliError> {
    let (owner, repo) = parse_repo(&args.repo)?;

    // Find or create a project for this repo
    let projects = client.list_projects().await.unwrap_or_default();
    let project_name = projects
        .iter()
        .find(|p| p.repo_url.as_deref().map(|u| u.contains(&format!("{owner}/{repo}"))).unwrap_or(false))
        .map(|p| p.name.clone())
        .unwrap_or_else(|| {
            // Auto-create project
            let name = format!("{owner}-{repo}");
            name
        });

    let input = StartTakeoverInput {
        repo_owner: owner,
        repo_name: repo,
        pr_number: args.pr as i64,
        max_errors: Some(args.max_errors),
    };

    let session = client.start_takeover(&project_name, &input).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&session).unwrap_or_default());
    } else {
        println!("Takeover started for PR #{}", session.pr_number);
        println!("  Project: {}", session.project_name);
        println!("  Repo: {}/{}", session.repo_owner, session.repo_name);
        println!("  Status: {}", session.status);
        println!("  Max errors: {}", session.max_errors);
        println!();
        println!("Use `looper takeover list` to monitor progress.");
        println!("Use `looper takeover stop --project {}` to stop.", session.project_name);
    }
    Ok(())
}

async fn list_takeovers(client: &DaemonAPIClient, json: bool) -> Result<(), CliError> {
    let sessions = client.list_takeovers().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&sessions).unwrap_or_default());
    } else if sessions.is_empty() {
        println!("No active takeover sessions.");
    } else {
        println!("{:<20} {:<15} {:<10} {:<10} {:<10}", "PROJECT", "PR", "STATUS", "PHASE", "CYCLES");
        println!("{}", "-".repeat(65));
        for s in &sessions {
            println!(
                "{:<20} #{:<14} {:<10} {:<10} {:<10}",
                s.project_name, s.pr_number, s.status, s.current_phase, s.cycles_completed
            );
        }
    }
    Ok(())
}

async fn stop_takeover(client: &DaemonAPIClient, args: &TakeoverStopArgs, json: bool) -> Result<(), CliError> {
    if let Some(ref project) = args.project {
        client.stop_takeover(project).await?;
        if json {
            println!(r#"{{"stopped":"{}"}}"#, project);
        } else {
            println!("Takeover stopped for project {project}.");
        }
    } else {
        // Stop all active takeovers
        let sessions = client.list_takeovers().await.unwrap_or_default();
        let active: Vec<_> = sessions.into_iter().filter(|s| s.status == "active").collect();
        if active.is_empty() {
            println!("No active takeover sessions to stop.");
            return Ok(());
        }
        for s in &active {
            client.stop_takeover(&s.project_name).await?;
        }
        if json {
            println!(r#"{{"stopped":{}}}"#, active.len());
        } else {
            println!("Stopped {} takeover session(s).", active.len());
        }
    }
    Ok(())
}

/// Parse "owner/repo" into (owner, repo).
fn parse_repo(repo: &str) -> Result<(String, String), CliError> {
    let parts: Vec<&str> = repo.split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(CliError::Other(format!("invalid repo format '{repo}': expected owner/repo")));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repo_valid() {
        assert_eq!(parse_repo("octocat/hello-world").unwrap(), ("octocat".into(), "hello-world".into()));
    }

    #[test]
    fn parse_repo_missing_slash() {
        assert!(parse_repo("invalid").is_err());
    }

    #[test]
    fn parse_repo_empty_owner() {
        assert!(parse_repo("/repo").is_err());
    }

    #[test]
    fn parse_repo_empty_name() {
        assert!(parse_repo("owner/").is_err());
    }
}
