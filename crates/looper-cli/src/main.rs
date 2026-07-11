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
    #[arg(long, global = true, default_value = looper_cli::commands::DEFAULT_DAEMON_BASE_URL)]
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

    // -- Disabled stubs (hidden; return unsupported if invoked) --
    #[command(subcommand, hide = true)]
    Review(commands::review::ReviewCommand),

    #[command(subcommand, hide = true)]
    Takeover(commands::takeover::TakeoverCommand),

    #[command(subcommand, hide = true)]
    RunStats(commands::run_stats::RunStatsCommand),

    #[command(subcommand, hide = true)]
    LogsFollow(commands::logs_follow::LogsFollowCommand),

    #[command(subcommand, hide = true)]
    Netadmin(commands::netadmin::NetadminCommand),

    #[command(subcommand, hide = true)]
    Labels(commands::labels::LabelsCommand),

    #[command(subcommand, hide = true)]
    Prompt(commands::prompt::PromptCommand),

    #[command(subcommand, hide = true)]
    Feedback(commands::feedback::FeedbackCommand),

    #[command(subcommand, hide = true)]
    Webhook(commands::webhook::WebhookCommand),

    #[command(subcommand, hide = true)]
    Diagnostics(commands::diagnostics::DiagnosticsCommand),

    // -- Worktree --
    #[command(subcommand)]
    Worktree(commands::worktree::WorktreeCommand),

    // -- PS (list active loops) --
    #[command(subcommand)]
    Ps(commands::ps::PsCommand),

    // -- Stop (stop a loop by seq) --
    #[command(subcommand)]
    Stop(commands::stop::StopCommand),

    // -- Jump (show worktree path) --
    #[command(subcommand)]
    Jump(commands::jump::JumpCommand),

    // -- PR list/show/status --
    #[command(subcommand)]
    Pr(commands::pr::PrCommand),

    // -- Bootstrap (first-run setup wizard) --
    #[command(subcommand)]
    Bootstrap(BootstrapCommand),

    /// Destructive: terminates ALL active/running loops (misnamed; hidden).
    /// Requires --i-know-this-terminates-all-active-loops.
    #[command(hide = true)]
    ReconcileStale {
        /// Confirm mass-terminate of every active/running loop (not only stale).
        #[arg(long = "i-know-this-terminates-all-active-loops")]
        confirm_terminate_all: bool,
    },
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
    Start {
        /// Path to config file
        #[arg(short = 'c', long)]
        config: Option<String>,
    },
    /// Stop the daemon
    Stop,
    /// Restart the daemon
    Restart {
        /// Path to config file
        #[arg(short = 'c', long)]
        config: Option<String>,
    },
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
pub enum BootstrapCommand {
    /// Run the bootstrap wizard
    Run,
    /// Check if already bootstrapped
    Status,
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
        // Stubs: no daemon ping — always unsupported (non-zero exit).
        Command::Review(cmd) => commands::review::handle(client, cmd, json).await,
        Command::Takeover(cmd) => commands::takeover::handle(client, cmd, json).await,
        Command::RunStats(cmd) => commands::run_stats::handle(client, cmd, json).await,
        Command::LogsFollow(cmd) => commands::logs_follow::handle(client, cmd, json).await,
        Command::Netadmin(cmd) => commands::netadmin::handle(client, cmd, json).await,
        Command::Labels(cmd) => commands::labels::handle(client, cmd, json).await,
        Command::Prompt(cmd) => commands::prompt::handle(client, cmd, json).await,
        Command::Feedback(cmd) => commands::feedback::handle(client, cmd, json).await,
        Command::Webhook(cmd) => commands::webhook::handle(client, cmd, json).await,
        Command::Diagnostics(cmd) => commands::diagnostics::handle(client, cmd, json).await,
        Command::Worktree(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::worktree::handle(client, cmd, json).await
        }
        Command::Ps(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::ps::handle(client, cmd, json).await
        }
        Command::Stop(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::stop::handle(client, cmd, json).await
        }
        Command::Jump(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::jump::handle(client, cmd, json).await
        }
        Command::Pr(cmd) => {
            commands::ensure_daemon(client).await?;
            commands::pr::handle(client, cmd, json).await
        }
        Command::Bootstrap(cmd) => run_bootstrap(cmd, json).await,
        Command::ReconcileStale { confirm_terminate_all } => {
            // Require confirm flag before touching the daemon (avoids silent mass-kill).
            if !*confirm_terminate_all {
                return commands::reconcile::handle(client, json, false).await;
            }
            commands::ensure_daemon(client).await?;
            commands::reconcile::handle(client, json, true).await
        }
    }
}

async fn run_health(client: &looper_cli::client::DaemonAPIClient, json: bool) -> Result<(), CliError> {
    match client.health().await {
        Ok(h) => {
            looper_cli::output::print_output(json, &h);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

async fn run_version(client: &looper_cli::client::DaemonAPIClient, json: bool) -> Result<(), CliError> {
    match client.server_version().await {
        Ok(v) => {
            looper_cli::output::print_output(json, &v);
            Ok(())
        }
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
        DaemonCommand::Start { config } => looper_cli::daemon::start(config.as_deref()).await,
        DaemonCommand::Stop => looper_cli::daemon::stop().await,
        DaemonCommand::Restart { config } => looper_cli::daemon::restart_with_config(config.as_deref()).await,
        DaemonCommand::Status => looper_cli::daemon::status().await,
        DaemonCommand::Logs { n } => looper_cli::daemon::logs(*n).await,
        DaemonCommand::Install { version } => looper_cli::daemon::install(version.clone()).await,
        DaemonCommand::Uninstall => looper_cli::daemon::uninstall().await,
    }
}

async fn run_bootstrap(cmd: &BootstrapCommand, json: bool) -> Result<(), CliError> {
    match cmd {
        BootstrapCommand::Run => commands::bootstrap::run(json).await,
        BootstrapCommand::Status => commands::bootstrap::status(json).await,
    }
}

async fn run_autoupgrade(cmd: &AutoupgradeCommand, _json: bool) -> Result<(), CliError> {
    match cmd {
        AutoupgradeCommand::Check => looper_cli::autoupgrade::check().await,
        AutoupgradeCommand::Status => looper_cli::autoupgrade::status().await,
        AutoupgradeCommand::Upgrade => looper_cli::autoupgrade::upgrade().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn default_daemon_url_is_7391() {
        let cli = Cli::try_parse_from(["looper", "health"]).expect("parse");
        assert_eq!(cli.daemon_url, "http://127.0.0.1:7391");
        assert!(!cli.daemon_url.contains("8080"));
    }

    #[test]
    fn help_surface_hides_stub_commands() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        // Primary surface must not advertise disabled stubs or destructive misnomer.
        for stub in [
            "takeover",
            "review",
            "run-stats",
            "logs-follow",
            "netadmin",
            "labels",
            "prompt",
            "feedback",
            "webhook",
            "diagnostics",
            "reconcile-stale",
        ] {
            // clap help lines list subcommands; avoid false positives on prose.
            let as_cmd_line = format!("  {stub} ");
            let as_cmd_eol = format!("  {stub}\n");
            assert!(
                !help.contains(&as_cmd_line) && !help.contains(stub),
                "main help must not list stub '{stub}':\n{help}"
            );
            let _ = as_cmd_eol;
        }
        // Sanity: real commands still visible.
        assert!(help.contains("health") || help.contains("Health"));
        assert!(help.contains("projects") || help.contains("Projects"));
    }

    #[test]
    fn stub_subcommands_still_parse_when_invoked() {
        // Hidden but invokable — must parse so handlers can return unsupported.
        assert!(Cli::try_parse_from(["looper", "takeover", "status", "r1"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "review", "status", "1"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "run-stats", "show", "r1"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "logs-follow", "status"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "netadmin", "status"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "labels", "status"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "prompt", "status"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "feedback", "status"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "webhook", "status"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "diagnostics", "status"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "reconcile-stale"]).is_ok());
        assert!(Cli::try_parse_from(["looper", "reconcile-stale", "--i-know-this-terminates-all-active-loops"]).is_ok());
    }
}
