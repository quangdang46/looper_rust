use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

// Suppress unused-crate-dependencies lint for deps used only by the library.
use chrono as _;
use reqwest as _;
use serde as _;
use serde_json as _;
use toml as _;
use which as _;

use looper_cli::commands;
use looper_cli::error::CliError;

#[derive(Debug, Parser)]
#[command(name = "looper", version, about = "Looper CLI — control the looper daemon")]
pub struct Cli {
    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Skip auto-upgrade check on startup
    #[arg(long, global = true)]
    no_auto_upgrade: bool,

    /// Daemon API base URL
    #[arg(long, global = true, default_value = "http://127.0.0.1:8080")]
    daemon_url: String,

    /// API token for daemon auth
    #[arg(long, global = true)]
    token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show daemon health
    Health,
    /// Show version info
    Version,
    /// Shutdown the daemon
    Shutdown,
    /// Reload daemon config
    Reload,

    // -- Projects --
    #[command(subcommand)]
    Projects(commands::projects::ProjectCommand),

    // -- Loops --
    #[command(subcommand)]
    Loops(commands::loops::LoopCommand),

    // -- Runs --
    #[command(subcommand)]
    Runs(commands::runs::RunCommand),

    // -- Queue --
    #[command(subcommand)]
    Queue(commands::queue::QueueCommand),

    // -- Events --
    #[command(subcommand)]
    Events(commands::events::EventCommand),

    // -- Locks --
    #[command(subcommand)]
    Locks(commands::locks::LockCommand),

    // -- Config --
    #[command(subcommand)]
    Config(commands::config::ConfigCommand),

    // -- Local config --
    #[command(subcommand)]
    ConfigLocal(ConfigLocalCommand),

    // -- Daemon lifecycle --
    #[command(subcommand)]
    Daemon(DaemonCommand),

    // -- Auto-upgrade --
    #[command(subcommand)]
    Autoupgrade(AutoupgradeCommand),

    // -- Review --
    #[command(subcommand)]
    Review(commands::review::ReviewCommand),

    // -- Takeover --
    #[command(subcommand)]
    Takeover(commands::takeover::TakeoverCommand),

    // -- Run Stats --
    #[command(subcommand)]
    RunStats(commands::run_stats::RunStatsCommand),

    // -- Logs Follow --
    #[command(subcommand)]
    LogsFollow(commands::logs_follow::LogsFollowCommand),

    // -- Netadmin --
    #[command(subcommand)]
    Netadmin(commands::netadmin::NetadminCommand),

    // -- Labels --
    #[command(subcommand)]
    Labels(commands::labels::LabelsCommand),

    // -- Prompt --
    #[command(subcommand)]
    Prompt(commands::prompt::PromptCommand),

    // -- Feedback --
    #[command(subcommand)]
    Feedback(commands::feedback::FeedbackCommand),

    // -- Webhook --
    #[command(subcommand)]
    Webhook(commands::webhook::WebhookCommand),

    // -- Diagnostics --
    #[command(subcommand)]
    Diagnostics(commands::diagnostics::DiagnosticsCommand),
}

#[derive(Debug, Subcommand)]
pub enum ConfigLocalCommand {
    /// Get a config value by key (e.g. server.host)
    Get { key: String },
    /// Set a config value by key
    Set { key: String, value: String },
    /// Unset a config key
    Unset { key: String },
    /// Open config in $EDITOR
    Edit,
    /// Migrate from legacy config format
    Migrate,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon
    Start,
    /// Stop the daemon
    Stop,
    /// Restart the daemon
    Restart,
    /// Check if daemon is running
    Status,
    /// Tail daemon logs
    Logs { n: Option<usize> },
    /// Install daemon as a system service (launchd on macOS, systemd on Linux)
    Install {
        /// Download a specific version from GitHub releases (e.g. v0.1.0)
        #[arg(long)]
        version: Option<String>,
    },
    /// Uninstall daemon: stop, remove service config, delete binary
    Uninstall,
}

#[derive(Debug, Subcommand)]
pub enum AutoupgradeCommand {
    /// Check for available updates
    Check,
    /// Show upgrade status
    Status,
    /// Perform the upgrade
    Upgrade,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::WARN.into()))
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let json = cli.json;

    let client = match commands::build_client(Some(&cli.daemon_url), cli.token.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    let result = run(&client, &cli.command, json).await;
    if let Err(e) = result {
        looper_cli::output::print_err(&e, json);
        std::process::exit(1);
    }
}

async fn run(client: &looper_cli::client::DaemonAPIClient, cmd: &Command, json: bool) -> Result<(), CliError> {
    match cmd {
        Command::Health => run_health(client, json).await,
        Command::Version => run_version(client, json).await,
        Command::Shutdown => {
            client.api_shutdown().await?;
            looper_cli::output::print_ok(json, "Daemon shutdown initiated");
            Ok(())
        }
        Command::Reload => {
            if !client.ping().await {
                return Err(CliError::daemon_not_running());
            }
            client.api_reload().await?;
            looper_cli::output::print_ok(json, "Daemon config reloaded");
            Ok(())
        }
        Command::Projects(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::projects::handle(client, cmd, json).await
        }
        Command::Loops(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::loops::handle(client, cmd, json).await
        }
        Command::Runs(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::runs::handle(client, cmd, json).await
        }
        Command::Queue(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::queue::handle(client, cmd, json).await
        }
        Command::Events(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::events::handle(client, cmd, json).await
        }
        Command::Locks(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::locks::handle(client, cmd, json).await
        }
        Command::Config(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::config::handle(client, cmd, json).await
        }
        Command::ConfigLocal(cmd) => run_config_local(cmd, json),
        Command::Daemon(cmd) => run_daemon(cmd, json).await,
        Command::Autoupgrade(cmd) => run_autoupgrade(cmd, json).await,
        Command::Review(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::review::handle(client, cmd, json).await
        }
        Command::Takeover(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::takeover::handle(client, cmd, json).await
        }
        Command::RunStats(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::run_stats::handle(client, cmd, json).await
        }
        Command::LogsFollow(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::logs_follow::handle(client, cmd, json).await
        }
        Command::Netadmin(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::netadmin::handle(client, cmd, json).await
        }
        Command::Labels(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::labels::handle(client, cmd, json).await
        }
        Command::Prompt(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::prompt::handle(client, cmd, json).await
        }
        Command::Feedback(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::feedback::handle(client, cmd, json).await
        }
        Command::Webhook(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::webhook::handle(client, cmd, json).await
        }
        Command::Diagnostics(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::diagnostics::handle(client, cmd, json).await
        }
    }
}

async fn run_health(client: &looper_cli::client::DaemonAPIClient, json: bool) -> Result<(), CliError> {
    match client.health().await {
        Ok(h) => { looper_cli::output::print_output(json, &h); Ok(()) }
        Err(e) => Err(e),
    }
}

async fn run_version(client: &looper_cli::client::DaemonAPIClient, json: bool) -> Result<(), CliError> {
    match client.server_version().await {
        Ok(v) => { looper_cli::output::print_output(json, &v); Ok(()) }
        Err(e) => Err(e),
    }
}

fn run_config_local(cmd: &ConfigLocalCommand, _json: bool) -> Result<(), CliError> {
    match cmd {
        ConfigLocalCommand::Get { key } => looper_cli::config_local::get(key),
        ConfigLocalCommand::Set { key, value } => looper_cli::config_local::set(key, value),
        ConfigLocalCommand::Unset { key } => looper_cli::config_local::unset(key),
        ConfigLocalCommand::Edit => looper_cli::config_local::edit(),
        ConfigLocalCommand::Migrate => looper_cli::config_local::migrate(),
    }
}

async fn run_daemon(cmd: &DaemonCommand, _json: bool) -> Result<(), CliError> {
    match cmd {
        DaemonCommand::Start => looper_cli::daemon::start().await,
        DaemonCommand::Stop => looper_cli::daemon::stop().await,
        DaemonCommand::Restart => looper_cli::daemon::restart().await,
        DaemonCommand::Status => looper_cli::daemon::status().await,
        DaemonCommand::Logs { n } => looper_cli::daemon::logs(*n).await,
        DaemonCommand::Install { version } => looper_cli::daemon::install(version.clone()).await,
        DaemonCommand::Uninstall => looper_cli::daemon::uninstall().await,
    }
}

async fn run_autoupgrade(cmd: &AutoupgradeCommand, _json: bool) -> Result<(), CliError> {
    match cmd {
        AutoupgradeCommand::Check => looper_cli::autoupgrade::check().await,
        AutoupgradeCommand::Status => looper_cli::autoupgrade::status().await,
        AutoupgradeCommand::Upgrade => looper_cli::autoupgrade::upgrade().await,
    }
}
