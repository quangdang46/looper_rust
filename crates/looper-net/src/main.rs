use clap::Parser;
use looper_net::{
    server::{build_router, Db, ServerState},
    types::NetConfig,
};
use std::sync::Arc;
use tokio::sync::broadcast;

// Dependencies used by the library crate (not directly by this binary)
#[allow(unused_imports)]
use {
    chrono as _, futures as _, looper_config as _, reqwest as _, rusqlite as _, serde as _, serde_json as _,
    thiserror as _, tokio_util as _,
};

/// Build-time version information.
mod version {
    pub fn value() -> &'static str {
        option_env!("LOOPER_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
    }
    pub fn channel() -> &'static str {
        option_env!("LOOPER_BUILD_CHANNEL").unwrap_or("release")
    }
    #[allow(dead_code)]
    pub fn api_version() -> &'static str {
        option_env!("LOOPER_BUILD_API_VERSION").unwrap_or("1")
    }
    pub fn git_commit_sha() -> &'static str {
        option_env!("LOOPER_BUILD_GIT_SHA").or(option_env!("VERGEN_GIT_SHA")).unwrap_or("unknown")
    }
    pub fn build_timestamp() -> &'static str {
        option_env!("LOOPER_BUILD_TIMESTAMP").unwrap_or("unknown")
    }
}

#[derive(Parser)]
#[command(name = "loopernet", version = version::value(), about = "LooperNet cloud server for multi-node coordination")]
struct Args {
    /// Listen address (default: 127.0.0.1:8089)
    #[arg(long, env = "LOOPERNET_LISTEN_ADDR", default_value = "127.0.0.1:8089")]
    listen_addr: String,

    /// SQLite database path
    #[arg(long, env = "LOOPERNET_DB_PATH")]
    db_path: String,

    /// Admin bearer token
    #[arg(long, env = "LOOPERNET_ADMIN_TOKEN")]
    admin_token: String,

    /// Network ID (human-readable name)
    #[arg(long, env = "LOOPERNET_NETWORK_ID")]
    network_id: String,

    /// Protocol version
    #[arg(long, env = "LOOPERNET_PROTOCOL_VERSION", default_value = "loopernet/v1")]
    protocol_version: String,

    /// Minimum daemon version
    #[arg(long, env = "LOOPERNET_MIN_DAEMON_VERSION")]
    minimum_daemon_version: Option<String>,

    /// Lease TTL in seconds
    #[arg(long, env = "LOOPERNET_LEASE_TTL_SECONDS", default_value = "30")]
    lease_ttl_seconds: u64,

    /// Advertise URL for webhook forwarding
    #[arg(long, env = "LOOPERNET_ADVERTISE_URL")]
    advertise_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();

    // Log version info
    tracing::info!(
        version = version::value(),
        channel = version::channel(),
        git_sha = version::git_commit_sha(),
        build_timestamp = version::build_timestamp(),
        "Starting LooperNet server"
    );

    // Build config
    let config = NetConfig {
        listen_addr: args.listen_addr.clone(),
        db_path: args.db_path.clone(),
        admin_token: args.admin_token.clone(),
        network_id: args.network_id.clone(),
        protocol_version: args.protocol_version.clone(),
        minimum_daemon_version: args.minimum_daemon_version.clone(),
        lease_ttl_seconds: args.lease_ttl_seconds,
        server_version: version::value().to_string(),
        advertise_url: args.advertise_url.clone(),
    };

    // Validate required config
    if config.db_path.is_empty() {
        eprintln!("ERROR: --db-path (or LOOPERNET_DB_PATH) is required");
        std::process::exit(1);
    }
    if config.admin_token.is_empty() {
        eprintln!("ERROR: --admin-token (or LOOPERNET_ADMIN_TOKEN) is required");
        std::process::exit(1);
    }

    // Open database
    let db = Db::new(&config.db_path)?;
    db.set_meta("network_id", &config.network_id)?;
    db.set_meta("protocol_version", &config.protocol_version)?;

    // Generate a webhook secret if not set
    if db.get_meta("webhook_secret")?.unwrap_or_default().is_empty() {
        let secret = uuid::Uuid::new_v4().to_string();
        db.set_meta("webhook_secret", &secret)?;
        tracing::info!("Generated webhook secret");
    }

    // Create broadcast channel for SSE events
    let (event_tx, _) = broadcast::channel::<looper_net::AuditEnvelope>(256);

    // Build server state
    let state = Arc::new(ServerState {
        db,
        admin_token: config.admin_token,
        network_id: config.network_id,
        protocol_version: config.protocol_version,
        minimum_daemon_version: config.minimum_daemon_version,
        lease_ttl_seconds: config.lease_ttl_seconds,
        server_version: config.server_version,
        advertise_url: config.advertise_url,
        event_tx,
    });

    // Build router and serve
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    tracing::info!("Listening on {}", config.listen_addr);

    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Shutdown signal received, exiting gracefully");
}
