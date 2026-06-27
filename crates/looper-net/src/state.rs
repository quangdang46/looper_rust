use crate::error::NetworkError;
use crate::types::LocalState;
use std::path::PathBuf;

/// Path to the local network state file (`~/.looper/network.json`).
fn default_state_path() -> Result<PathBuf, NetworkError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| NetworkError::Other("cannot determine home directory".to_string()))?;
    Ok(PathBuf::from(home).join(".looper").join("network.json"))
}

/// Save local state to disk (0600 permissions).
pub fn save_state(state: &LocalState) -> Result<(), NetworkError> {
    save_state_at(state, &default_state_path()?)
}

/// Save local state to a specific path.
pub fn save_state_at(state: &LocalState, path: &PathBuf) -> Result<(), NetworkError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state)?;
    // Write to temp file then atomically rename
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Load local state from disk.
pub fn load_state() -> Result<Option<LocalState>, NetworkError> {
    load_state_from(&default_state_path()?)
}

/// Load local state from a specific path.
pub fn load_state_from(path: &PathBuf) -> Result<Option<LocalState>, NetworkError> {
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(path)?;
    let state: LocalState = serde_json::from_str(&json)?;
    Ok(Some(state))
}

/// Remove the local state file (when leaving a network).
pub fn remove_state() -> Result<(), NetworkError> {
    let path = default_state_path()?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GitHubIdentity;

    #[test]
    fn test_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("looper-net-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = dir.join("network.json");
        let state = LocalState {
            url: "https://cloud.example.com".to_string(),
            network_id: "net_abc".to_string(),
            node_id: "node_xyz".to_string(),
            node_name: "my-node".to_string(),
            node_token: "token_123".to_string(),
            github: GitHubIdentity { numeric_id: 12345, login: "testuser".to_string() },
        };

        save_state_at(&state, &path).unwrap();
        let loaded = load_state_from(&path).unwrap().unwrap();

        assert_eq!(loaded.url, state.url);
        assert_eq!(loaded.network_id, state.network_id);
        assert_eq!(loaded.node_id, state.node_id);
        assert_eq!(loaded.node_name, state.node_name);
        assert_eq!(loaded.node_token, state.node_token);
        assert_eq!(loaded.github.numeric_id, 12345);

        assert!(path.exists());
        std::fs::remove_file(&path).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
