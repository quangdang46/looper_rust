use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::error::CliError;

/// Determine the config directory (~/.config/looper or $XDG_CONFIG_HOME/looper).
pub fn config_dir() -> Result<PathBuf, CliError> {
    let base = std::env::var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".config")
    });
    Ok(base.join("looper"))
}

/// Path to the primary config file.
pub fn config_file_path() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("config.toml"))
}

/// Read raw config file content as a string.
pub fn read_raw() -> Result<String, CliError> {
    let path = config_file_path()?;
    if !path.exists() {
        return Err(CliError::config("config file not found"));
    }
    fs::read_to_string(&path).map_err(CliError::from)
}

/// Write raw string content to the config file (creates directory if needed).
pub fn write_raw(content: &str) -> Result<(), CliError> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    fs::write(&path, content)?;
    Ok(())
}

/// Get a config value by dotted key path (e.g. "server.host").
pub fn get(key: &str) -> Result<(), CliError> {
    let raw = read_raw()?;
    let value: Value = toml::from_str(&raw).map_err(|e| CliError::config(format!("parse error: {e}")))?;
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = &value;
    for part in &parts {
        match current {
            Value::Object(map) => {
                current = map.get(*part).ok_or_else(|| CliError::config(format!("key '{key}' not found")))?;
            }
            _ => return Err(CliError::config(format!("cannot traverse into {current:?}"))),
        }
    }
    println!("{}", current);
    Ok(())
}

/// Set a config value by dotted key path. Creates intermediate maps.
pub fn set(key: &str, raw_value: &str) -> Result<(), CliError> {
    let raw = if config_file_path()?.exists() { read_raw()? } else { String::new() };
    let mut value: Value = if raw.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        toml::from_str(&raw).map_err(|e| CliError::config(format!("parse error: {e}")))?
    };

    // Parse the raw value as TOML to get typed value
    let parsed_toml: toml::Value = toml::from_str(&format!("x = {raw_value}"))
        .map(|v: toml::Value| {
            let toml::Value::Table(mut t) = v else { unreachable!() };
            t.remove("x").unwrap_or_else(|| toml::Value::String(raw_value.into()))
        })
        .unwrap_or_else(|_| toml::Value::String(raw_value.into()));

    // Convert TOML value to JSON value via serde_json
    let parsed_json: Value =
        serde_json::to_value(&parsed_toml).map_err(|e| CliError::config(format!("conversion error: {e}")))?;

    set_nested(&mut value, key, parsed_json)?;

    // Serialize back to TOML
    let toml_value = json_to_toml(&value).map_err(|e| CliError::config(format!("serialization error: {e}")))?;
    let out = toml::to_string(&toml_value).map_err(|e| CliError::config(format!("TOML serialization error: {e}")))?;
    write_raw(&out)?;
    println!("Set {key} = {raw_value}");
    Ok(())
}

/// Unset (delete) a config key by dotted path.
pub fn unset(key: &str) -> Result<(), CliError> {
    let raw = read_raw()?;
    let mut value: Value = toml::from_str(&raw).map_err(|e| CliError::config(format!("parse error: {e}")))?;
    remove_nested(&mut value, key)?;
    let toml_value = json_to_toml(&value).map_err(|e| CliError::config(format!("serialization error: {e}")))?;
    let out = toml::to_string(&toml_value).map_err(|e| CliError::config(format!("TOML serialization error: {e}")))?;
    write_raw(&out)?;
    println!("Unset {key}");
    Ok(())
}

/// Open the config file in $EDITOR.
#[allow(clippy::disallowed_methods)]
pub fn edit() -> Result<(), CliError> {
    let path = config_file_path()?;
    if !path.exists() {
        // Create default empty config
        write_raw("# Looper configuration\n")?;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .map_err(|e| CliError::config(format!("failed to launch editor '{editor}': {e}")))?;
    if !status.success() {
        return Err(CliError::config("editor exited with non-zero status"));
    }
    Ok(())
}

/// Migrate legacy config if detected.
pub fn migrate() -> Result<(), CliError> {
    let legacy = config_dir()?.join("config.json");
    if !legacy.exists() {
        println!("No legacy config found at {}", legacy.display());
        return Ok(());
    }
    let json_raw = fs::read_to_string(&legacy)?;
    let json_value: Value = serde_json::from_str(&json_raw)?;
    let toml_value = json_to_toml(&json_value).map_err(|e| CliError::config(format!("conversion error: {e}")))?;
    let out = toml::to_string_pretty(&toml_value).map_err(|e| CliError::config(format!("TOML error: {e}")))?;
    write_raw(&out)?;

    // Backup legacy
    let backup = legacy.with_extension("json.bak");
    fs::rename(&legacy, &backup)?;
    println!("Migrated {} → config.toml (backup at {})", legacy.display(), backup.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn set_nested(root: &mut Value, key: &str, val: Value) -> Result<(), CliError> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = root;
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            // Leaf: set the value
            match current {
                Value::Object(map) => {
                    map.insert((*part).to_string(), val);
                }
                _ => return Err(CliError::config(format!("cannot set key '{key}'"))),
            }
            break;
        } else {
            // Intermediate: ensure map exists
            match current {
                Value::Object(map) => {
                    if !map.contains_key(*part) {
                        map.insert((*part).to_string(), Value::Object(serde_json::Map::new()));
                    }
                    current = map.get_mut(*part).unwrap();
                }
                _ => return Err(CliError::config(format!("cannot traverse '{key}'"))),
            }
        }
    }
    Ok(())
}

fn remove_nested(root: &mut Value, key: &str) -> Result<(), CliError> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = root;
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            match current {
                Value::Object(map) => {
                    map.remove(*part);
                }
                _ => return Err(CliError::config(format!("cannot unset key '{key}'"))),
            }
        } else {
            match current {
                Value::Object(map) => {
                    current = map.get_mut(*part).ok_or_else(|| CliError::config(format!("key '{key}' not found")))?;
                }
                _ => return Err(CliError::config(format!("cannot traverse '{key}'"))),
            }
        }
    }
    Ok(())
}

/// Best-effort conversion from serde_json::Value to toml::Value.
fn json_to_toml(v: &Value) -> Result<toml::Value, String> {
    match v {
        Value::Null => Ok(toml::Value::String("null".into())),
        Value::Bool(b) => Ok(toml::Value::Boolean(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(toml::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(toml::Value::Float(f))
            } else {
                Err("unsupported number".into())
            }
        }
        Value::String(s) => Ok(toml::Value::String(s.clone())),
        Value::Array(arr) => {
            let items: Result<Vec<_>, _> = arr.iter().map(json_to_toml).collect();
            Ok(toml::Value::Array(items?))
        }
        Value::Object(map) => {
            let mut table = toml::map::Map::new();
            for (k, v) in map {
                table.insert(k.clone(), json_to_toml(v)?);
            }
            Ok(toml::Value::Table(table))
        }
    }
}
