#[cfg(unix)]
use crate::error::CliError;
use std::path::PathBuf;
use std::process::Stdio;

/// Detect the looperd binary path (same dir as looper-cli, or in PATH).
fn daemon_binary() -> Result<PathBuf, CliError> {
    // First check same directory as current executable
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.parent().map(|p| p.join("looperd"));
        if let Some(path) = sibling {
            if path.exists() {
                return Ok(path);
            }
        }
    }
    // Fall back to PATH
    which::which("looperd")
        .map_err(|_| CliError::daemon_lifecycle("looperd binary not found (not in PATH and not alongside looper-cli)"))
}

/// Default log file path.
fn log_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local").join("share").join("looper").join("daemon.log")
}

/// Start the daemon in the background.
#[allow(clippy::disallowed_methods)]
pub async fn start(config_path: Option<&str>) -> Result<(), CliError> {
    let bin = daemon_binary()?;
    let log = log_file();

    // Create log directory
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_file = log.clone();
    let mut cmd = std::process::Command::new(&bin);
    cmd.stdout(Stdio::from(
        std::fs::File::create(&log)
            .map_err(|e| CliError::daemon_lifecycle(format!("cannot create log file {log:?}: {e}")))?,
    ))
    .stderr(Stdio::from(
        std::fs::File::options()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(|e| CliError::daemon_lifecycle(format!("cannot open log file {log:?}: {e}")))?,
    ))
    .stdin(Stdio::null());

    if let Some(cfg) = config_path {
        cmd.arg("--config");
        cmd.arg(cfg);
    }

    let child = cmd.spawn().map_err(|e| CliError::daemon_lifecycle(format!("cannot start daemon: {e}")))?;

    println!("looperd started (PID {}), log: {}", child.id(), log_file.display());
    Ok(())
}

/// Stop the daemon by finding and killing the looperd process.
#[allow(clippy::disallowed_methods)]
pub async fn stop() -> Result<(), CliError> {
    let output = std::process::Command::new("pkill")
        .arg("-f")
        .arg("looperd")
        .output()
        .map_err(|e| CliError::daemon_lifecycle(format!("cannot stop daemon: {e}")))?;

    if output.status.success() {
        println!("looperd stopped");
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not found") || stderr.contains("no process") {
            println!("looperd is not running");
        } else {
            return Err(CliError::daemon_lifecycle(format!("failed to stop daemon: {stderr}")));
        }
    }
    Ok(())
}

/// Restart the daemon.
pub async fn restart_with_config(config_path: Option<&str>) -> Result<(), CliError> {
    stop().await?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    start(config_path).await
}

/// Print daemon status (check if looperd process is running).
#[allow(clippy::disallowed_methods)]
pub async fn status() -> Result<(), CliError> {
    let output = std::process::Command::new("pgrep")
        .arg("-f")
        .arg("looperd")
        .output()
        .map_err(|e| CliError::daemon_lifecycle(format!("cannot check status: {e}")))?;

    if output.status.success() {
        let pid_str = String::from_utf8_lossy(&output.stdout);
        let pid = pid_str.trim();
        println!("looperd is running (PID {pid})");
    } else {
        println!("looperd is not running");
    }
    Ok(())
}

/// Tail the daemon log file.
#[allow(clippy::disallowed_methods)]
pub async fn logs(lines: Option<usize>) -> Result<(), CliError> {
    let log = log_file();
    if !log.exists() {
        return Err(CliError::daemon_lifecycle(format!("log file not found at {}", log.display())));
    }
    let n = lines.unwrap_or(50).to_string();
    let status = std::process::Command::new("tail")
        .arg("-n")
        .arg(&n)
        .arg(&log)
        .status()
        .map_err(|e| CliError::daemon_lifecycle(format!("cannot tail log: {e}")))?;
    if !status.success() {
        return Err(CliError::daemon_lifecycle("tail command failed"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Install / Uninstall
// ---------------------------------------------------------------------------

/// Install the daemon as a platform-appropriate service.
///
/// On macOS, generates a launchd plist and loads it.
/// On Linux, generates a systemd service unit and enables it.
/// On other platforms, prints a message.
///
/// If `version` is `Some`, downloads the specified release from GitHub first.
#[allow(clippy::disallowed_methods)]
pub async fn install(version: Option<String>) -> Result<(), CliError> {
    let bin_path = if let Some(ref ver) = version {
        download_version(ver).await?
    } else {
        std::env::current_exe().map_err(|e| CliError::daemon_lifecycle(format!("cannot find current binary: {e}")))?
    };

    #[cfg(target_os = "macos")]
    {
        install_launchd(&bin_path).await?;
    }

    #[cfg(target_os = "linux")]
    {
        install_systemd(&bin_path).await?;
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("Unsupported platform. Daemon binary is at: {}", bin_path.display());
        println!("Please install manually.");
    }

    Ok(())
}

/// Uninstall the daemon: stop, remove service config, delete binary.
#[allow(clippy::disallowed_methods)]
pub async fn uninstall() -> Result<(), CliError> {
    // 1. Stop the running daemon
    let _ = stop().await;

    // 2. Remove platform service configuration
    #[cfg(target_os = "macos")]
    {
        uninstall_launchd().await?;
    }

    #[cfg(target_os = "linux")]
    {
        uninstall_systemd().await?;
    }

    // 3. Remove binary files from known install locations
    let install_dirs = install_directories();
    let mut removed_any = false;
    for dir in &install_dirs {
        let bin_path = dir.join("looperd");
        if bin_path.exists() {
            std::fs::remove_file(&bin_path)
                .map_err(|e| CliError::daemon_lifecycle(format!("cannot remove {}: {e}", bin_path.display())))?;
            removed_any = true;
            println!("Removed: {}", bin_path.display());
        }
        let cli_path = dir.join("looper");
        if cli_path.exists() {
            std::fs::remove_file(&cli_path)
                .map_err(|e| CliError::daemon_lifecycle(format!("cannot remove {}: {e}", cli_path.display())))?;
            removed_any = true;
            println!("Removed: {}", cli_path.display());
        }
    }

    // 4. Remove data directories if empty
    if let Some(data_dir) = data_directory() {
        if data_dir.exists() {
            // Only remove if it's empty or nearly so (just daemon state)
            let _ = std::fs::remove_dir_all(&data_dir).map_err(|e| {
                CliError::daemon_lifecycle(format!("warning: could not remove data dir {}: {e}", data_dir.display()))
            });
        }
    }

    if removed_any {
        println!("looper daemon uninstalled successfully.");
    } else {
        println!("No looper binaries found at expected locations.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Version download
// ---------------------------------------------------------------------------

/// Download a specific version of looperd from GitHub releases.
#[allow(clippy::disallowed_methods)]
async fn download_version(version: &str) -> Result<PathBuf, CliError> {
    let install_dir = install_directories().first().cloned().unwrap_or_else(|| PathBuf::from("/usr/local/bin"));

    std::fs::create_dir_all(&install_dir)?;

    // Detect platform triple (shared with autoupgrade RELEASE_REPO conventions)
    let triple = crate::autoupgrade::platform_triple().ok_or_else(|| {
        CliError::daemon_lifecycle(format!("unsupported platform: {}/{}", std::env::consts::OS, std::env::consts::ARCH))
    })?;

    let asset_name = crate::autoupgrade::asset_name_for_triple("looperd", triple);
    let url =
        format!("https://github.com/{}/releases/download/{version}/{asset_name}", crate::autoupgrade::RELEASE_REPO);

    println!("Downloading looperd {version} for {triple}...");
    let client = reqwest::Client::builder()
        .user_agent("looper-cli")
        .build()
        .map_err(|e| CliError::daemon_lifecycle(format!("HTTP client error: {e}")))?;

    let response =
        client.get(&url).send().await.map_err(|e| CliError::daemon_lifecycle(format!("download failed: {e}")))?;

    if !response.status().is_success() {
        return Err(CliError::daemon_lifecycle(format!("download returned {} for {url}", response.status())));
    }

    let bytes = response.bytes().await.map_err(|e| CliError::daemon_lifecycle(format!("read response: {e}")))?;

    let dest = install_dir.join("looperd");
    std::fs::write(&dest, &bytes)
        .map_err(|e| CliError::daemon_lifecycle(format!("write binary to {}: {e}", dest.display())))?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| CliError::daemon_lifecycle(format!("chmod binary: {e}")))?;
    }

    println!("Downloaded looperd {version} to {}", dest.display());
    Ok(dest)
}

// ---------------------------------------------------------------------------
// macOS launchd
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
async fn install_launchd(bin_path: &std::path::Path) -> Result<(), CliError> {
    let home = std::env::var("HOME").map_err(|_| CliError::daemon_lifecycle("$HOME not set"))?;

    let plist_dir = format!("{home}/Library/LaunchAgents");
    std::fs::create_dir_all(&plist_dir)?;

    let plist_path = format!("{plist_dir}/io.looper.daemon.plist");
    let bin_display = bin_path.display();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>io.looper.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin_display}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{home}/.local/share/looper/daemon.log</string>
    <key>StandardErrorPath</key>
    <string>{home}/.local/share/looper/daemon.log</string>
</dict>
</plist>"#
    );

    std::fs::write(&plist_path, &plist).map_err(|e| CliError::daemon_lifecycle(format!("cannot write plist: {e}")))?;

    let status = std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path])
        .status()
        .map_err(|e| CliError::daemon_lifecycle(format!("cannot load launchd job: {e}")))?;

    if status.success() {
        println!("Installed launchd service at {plist_path}");
    } else {
        return Err(CliError::daemon_lifecycle("launchctl load failed"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn uninstall_launchd() -> Result<(), CliError> {
    let home = std::env::var("HOME").map_err(|_| CliError::daemon_lifecycle("$HOME not set"))?;

    let plist_path = format!("{home}/Library/LaunchAgents/io.looper.daemon.plist");

    // Unload if loaded
    if std::path::Path::new(&plist_path).exists() {
        let _ = std::process::Command::new("launchctl").args(["unload", "-w", &plist_path]).status();
        std::fs::remove_file(&plist_path)
            .map_err(|e| CliError::daemon_lifecycle(format!("cannot remove plist: {e}")))?;
        println!("Removed launchd plist: {plist_path}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Linux systemd
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
async fn install_systemd(bin_path: &std::path::Path) -> Result<(), CliError> {
    let user = std::env::var("USER").unwrap_or_else(|_| "root".into());
    let bin_display = bin_path.display();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());

    // Compute log path consistent with the start() default
    let log_path = format!("{home}/.local/share/looper/daemon.log");

    let unit_content = format!(
        r#"[Unit]
Description=Looper Daemon
After=network.target

[Service]
Type=simple
ExecStart={bin_display}
User={user}
Restart=on-failure
RestartSec=5
StandardOutput=append:{log_path}
StandardError=append:{log_path}

[Install]
WantedBy=multi-user.target
"#
    );

    // System-level install requires root; offer user-level if that fails
    let systemd_paths =
        ["/etc/systemd/system/looperd.service".to_string(), format!("{home}/.config/systemd/user/looperd.service")];

    let mut written = false;
    for path_str in &systemd_paths {
        let path = std::path::Path::new(path_str);
        if let Some(parent) = path.parent() {
            if parent.exists() {
                std::fs::write(path, &unit_content).map_err(|e| {
                    CliError::daemon_lifecycle(format!("cannot write systemd unit {}: {e}", path.display()))
                })?;
                println!("Wrote systemd unit: {}", path.display());
                written = true;
                break;
            }
        }
    }

    if !written {
        // Fallback: try user-level systemd
        let user_dir = format!("{home}/.config/systemd/user");
        std::fs::create_dir_all(&user_dir)?;
        let fallback_path = format!("{user_dir}/looperd.service");
        std::fs::write(&fallback_path, &unit_content)
            .map_err(|e| CliError::daemon_lifecycle(format!("cannot write systemd unit {}: {e}", fallback_path)))?;
        println!("Wrote systemd unit: {fallback_path}");
    }

    // Enable and start
    let _ = std::process::Command::new("systemctl").args(["enable", "looperd.service"]).status();
    let _ = std::process::Command::new("systemctl").args(["start", "looperd.service"]).status();

    println!("systemd unit installed. Run 'systemctl status looperd.service' to check.");
    Ok(())
}

#[cfg(target_os = "linux")]
async fn uninstall_systemd() -> Result<(), CliError> {
    // Disable and stop
    let _ = std::process::Command::new("systemctl").args(["disable", "looperd.service"]).status();
    let _ = std::process::Command::new("systemctl").args(["stop", "looperd.service"]).status();

    // Remove unit files
    let unit_paths = [
        PathBuf::from("/etc/systemd/system/looperd.service"),
        PathBuf::from("/usr/lib/systemd/system/looperd.service"),
    ];
    for path in &unit_paths {
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| {
                CliError::daemon_lifecycle(format!("cannot remove systemd unit {}: {e}", path.display()))
            })?;
            println!("Removed systemd unit: {}", path.display());
        }
    }

    // User-level
    if let Ok(home) = std::env::var("HOME") {
        let user_path = PathBuf::from(home).join(".config").join("systemd").join("user").join("looperd.service");
        if user_path.exists() {
            std::fs::remove_file(&user_path)
                .map_err(|e| CliError::daemon_lifecycle(format!("cannot remove user systemd unit: {e}",)))?;
            println!("Removed user systemd unit: {}", user_path.display());
        }
    }

    println!("systemd service removed.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Install directories to search for binary removal during uninstall.
fn install_directories() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    dirs.push(PathBuf::from("/usr/local/bin"));
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(&home).join(".local").join("bin"));
        dirs.push(PathBuf::from(home).join("bin"));
    }
    dirs
}

/// Data directory where looper stores its runtime state.
fn data_directory() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".local").join("share").join("looper"))
}
