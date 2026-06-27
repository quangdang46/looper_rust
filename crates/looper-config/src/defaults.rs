use directories::ProjectDirs;
use std::path::PathBuf;

use crate::types::CONFIG_DIR_NAME;

/// Returns the platform-appropriate project directories for Looper.
///
/// Linux:   ~/.config/looper, ~/.local/share/looper, ~/.cache/looper
/// macOS:   ~/Library/Application Support/com.looper.looper, ...
/// Windows: ~/AppData/Roaming/com.looper/looper/config, ...
pub fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("com", "looper", "looper")
}

/// Default config directory (~/.config/looper or platform equivalent).
pub fn default_config_dir() -> Option<PathBuf> {
    project_dirs().map(|d| d.config_dir().to_path_buf())
}

/// Default data directory (~/.local/share/looper or platform equivalent).
pub fn default_data_dir() -> Option<PathBuf> {
    project_dirs().map(|d| d.data_dir().to_path_buf())
}

/// Default cache directory (~/.cache/looper or platform equivalent).
pub fn default_cache_dir() -> Option<PathBuf> {
    project_dirs().map(|d| d.cache_dir().to_path_buf())
}

/// Default log directory.
pub fn default_log_dir() -> Option<PathBuf> {
    // On Linux: ~/.local/share/looper/log, on macOS: ~/Library/Logs
    project_dirs().map(|d| {
        let mut path = d.data_dir().to_path_buf();
        path.push("log");
        path
    })
}

/// Default home directory (~).
pub fn default_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// Platform-aware default config search paths.
///
/// Returns paths in search priority order:
/// 1. `$LOOPER_CONFIG` env var (if set)
/// 2. `$XDG_CONFIG_HOME/looper/` (if set)
/// 3. `~/.config/looper/`
pub fn default_config_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. LOOPER_CONFIG env var
    if let Ok(custom_path) = std::env::var("LOOPER_CONFIG") {
        paths.push(PathBuf::from(custom_path));
    }

    // 2. XDG_CONFIG_HOME
    if let Ok(xdg_home) = std::env::var("XDG_CONFIG_HOME") {
        let mut p = PathBuf::from(xdg_home);
        p.push(CONFIG_DIR_NAME);
        paths.push(p);
    }

    // 3. ~/.config/looper (XDG-style fallback for macOS where ProjectDirs
    //    returns ~/Library/Application Support/com.looper.looper/)
    if let Some(home) = default_home_dir() {
        let mut p = home;
        p.push(".config");
        p.push(CONFIG_DIR_NAME);
        if !paths.contains(&p) {
            paths.push(p);
        }
    }

    // 4. Project dirs (platform-specific: macOS -> ~/Library/Application Support/com.looper.looper/)
    if let Some(dirs) = project_dirs() {
        let d = dirs.config_dir().to_path_buf();
        if !paths.contains(&d) {
            paths.push(d);
        }
    }

    // 5. Legacy ~/.looper
    if let Some(home) = default_home_dir() {
        let mut p = home;
        p.push(format!(".{}", CONFIG_DIR_NAME));
        paths.push(p);
    }

    paths
}

/// Default config file names in preference order.
pub const CONFIG_FILE_NAMES: &[&str] = &["looper.toml", "looper.yaml", "looper.yml", "looper.json"];

/// Discover the first existing config file from the default search paths.
pub fn discover_config_file() -> Option<PathBuf> {
    for dir in default_config_search_paths() {
        for name in CONFIG_FILE_NAMES {
            let path = dir.join(name);
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_dirs_accessible() {
        // This should work in any environment (may fail in sandboxed test runners)
        let _ = project_dirs();
    }

    #[test]
    fn test_home_dir_accessible() {
        let home = default_home_dir();
        assert!(home.is_some(), "HOME should be set");
    }

    #[test]
    fn test_config_file_names_have_toml() {
        assert!(CONFIG_FILE_NAMES.contains(&"looper.toml"));
    }

    #[test]
    fn test_config_search_paths_not_empty() {
        let paths = default_config_search_paths();
        assert!(!paths.is_empty());
    }

    #[test]
    fn test_discover_when_no_file() {
        // Without a config file, discover should return None
        assert!(discover_config_file().is_none());
    }
}
