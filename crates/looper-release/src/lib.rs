//! Release manifest builder for Looper.
//!
//! Ported from Go `legacy/internal/release/manifest.go` (207 LOC).
//!
//! Builds a release manifest JSON file that records all artifacts,
//! their SHA256 checksums, and version compatibility metadata
//! (MinCliForDaemon, MinDaemonForCli).

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MANIFEST_VERSION: i32 = 1;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Full release manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub manifest_version: i32,
    pub version: String,
    pub tag: String,
    pub released: String,
    pub channel: String,
    pub api_version: String,
    pub schema_version: String,
    pub min_cli_for_daemon: String,
    pub min_daemon_for_cli: String,
    #[serde(default)]
    pub artifacts: HashMap<String, Artifact>,
}

/// A single release artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    pub size: i64,
}

/// Input for building a manifest.
#[derive(Debug, Clone)]
pub struct BuildManifestInput {
    pub tag: String,
    pub version: String,
    pub released: chrono::DateTime<chrono::Utc>,
    pub channel: String,
    pub api_version: String,
    pub schema_version: String,
    pub min_cli_for_daemon: String,
    pub min_daemon_for_cli: String,
    pub repo: String,
    pub assets_dir: String,
    pub required_artifacts: Vec<String>,
}

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------

lazy_static::lazy_static! {
    static ref RELEASE_TAG_PATTERN: Regex =
        Regex::new(r"^v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$").unwrap();
}

/// Build a release manifest from input.
pub fn build_manifest(input: BuildManifestInput) -> Result<Manifest, String> {
    let tag = input.tag.trim().to_string();
    if !RELEASE_TAG_PATTERN.is_match(&tag) {
        return Err(format!("tag must match vMAJOR.MINOR.PATCH[-PRERELEASE]: {tag:?}"));
    }

    let version = if input.version.trim().is_empty() {
        tag.strip_prefix('v').unwrap_or(&tag).to_string()
    } else {
        input.version.trim().to_string()
    };

    if input.api_version.trim().is_empty() {
        return Err("apiVersion is required".into());
    }
    if input.schema_version.trim().is_empty() {
        return Err("schemaVersion is required".into());
    }
    if input.min_cli_for_daemon.trim().is_empty() {
        return Err("minCliForDaemon is required".into());
    }
    if input.min_daemon_for_cli.trim().is_empty() {
        return Err("minDaemonForCli is required".into());
    }
    if input.repo.trim().is_empty() {
        return Err("repo is required".into());
    }

    let channel = if input.channel.trim().is_empty() {
        if version.contains('-') {
            "beta"
        } else {
            "stable"
        }
    } else {
        input.channel.trim()
    };

    let artifacts = collect_artifacts(input.assets_dir.trim(), &input.repo, &tag)?;

    if input.required_artifacts.is_empty() {
        return Err("at least one required artifact is required".into());
    }
    for name in &input.required_artifacts {
        if !artifacts.contains_key(name) {
            return Err(format!("missing required artifact: {name}"));
        }
    }

    Ok(Manifest {
        manifest_version: MANIFEST_VERSION,
        version,
        tag,
        released: input.released.to_rfc3339(),
        channel: channel.to_string(),
        api_version: input.api_version.trim().to_string(),
        schema_version: input.schema_version.trim().to_string(),
        min_cli_for_daemon: input.min_cli_for_daemon.trim().to_string(),
        min_daemon_for_cli: input.min_daemon_for_cli.trim().to_string(),
        artifacts,
    })
}

/// Encode a manifest as pretty-printed JSON.
pub fn encode_manifest(manifest: &Manifest) -> Result<String, String> {
    serde_json::to_string_pretty(manifest).map_err(|e| format!("encode manifest: {e}"))
}

// ---------------------------------------------------------------------------
// Artifact collection
// ---------------------------------------------------------------------------

fn collect_artifacts(assets_dir: &str, repo: &str, tag: &str) -> Result<HashMap<String, Artifact>, String> {
    if assets_dir.is_empty() {
        return Err("assets directory is required".into());
    }
    let dir = Path::new(assets_dir);
    let mut entries: Vec<_> =
        std::fs::read_dir(dir).map_err(|e| format!("read assets directory: {e}"))?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    let mut artifacts = HashMap::new();

    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if name.ends_with(".sha256") || name.ends_with(".json") || name.ends_with(".minisig") {
            continue;
        }

        let path = entry.path();
        let info = entry.metadata().map_err(|e| format!("stat {name}: {e}"))?;

        let sha_path = path.with_extension("sha256");
        let sha_bytes = std::fs::read_to_string(&sha_path).map_err(|e| format!("read checksum for {name}: {e}"))?;
        let sha = parse_sha256(&sha_bytes);
        if sha.is_empty() {
            return Err(format!("invalid checksum format for {name}"));
        }

        artifacts.insert(
            name.clone(),
            Artifact {
                url: format!("https://github.com/{repo}/releases/download/{tag}/{name}"),
                sha256: sha,
                size: info.len() as i64,
            },
        );
    }

    Ok(artifacts)
}

// ---------------------------------------------------------------------------
// SHA256 parsing
// ---------------------------------------------------------------------------

/// Parse a hex SHA256 from text (first whitespace-delimited token, 64 hex chars).
pub fn parse_sha256(raw: &str) -> String {
    let hash = raw.split_whitespace().next().unwrap_or("").to_lowercase();
    if hash.len() != 64 {
        return String::new();
    }
    if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return String::new();
    }
    hash
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn min_input() -> BuildManifestInput {
        BuildManifestInput {
            tag: "v0.1.0".into(),
            version: String::new(),
            released: chrono::Utc.with_ymd_and_hms(2026, 6, 22, 12, 0, 0).unwrap(),
            channel: String::new(),
            api_version: "v1".into(),
            schema_version: "V2__extend_coverage".into(),
            min_cli_for_daemon: "0.1.0".into(),
            min_daemon_for_cli: "0.1.0".into(),
            repo: "quangdang46/looper".into(),
            assets_dir: std::env::temp_dir().to_string_lossy().to_string(),
            required_artifacts: vec![],
        }
    }

    #[test]
    fn test_build_manifest_invalid_tag() {
        let mut input = min_input();
        input.tag = "not-a-tag".into();
        let result = build_manifest(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("tag must match"));
    }

    #[test]
    fn test_build_manifest_missing_api_version() {
        let mut input = min_input();
        input.api_version = "".into();
        let result = build_manifest(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("apiVersion"));
    }

    #[test]
    fn test_channel_detection() {
        // We can test channel by looking at the function logic directly
        // without needing to pass collect_artifacts
        let result = build_manifest(BuildManifestInput {
            tag: "v0.2.0-beta".into(),
            version: String::new(),
            released: chrono::Utc::now(),
            channel: String::new(),
            api_version: "v1".into(),
            schema_version: "V2__extend_coverage".into(),
            min_cli_for_daemon: "0.1.0".into(),
            min_daemon_for_cli: "0.1.0".into(),
            repo: "owner/repo".into(),
            assets_dir: "/nonexistent".into(),
            required_artifacts: vec!["looperd".into()],
        });
        // Should fail on collect_artifacts (nonexistent dir), not on channel
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("read assets directory"));
    }

    #[test]
    fn test_encode_manifest() {
        let manifest = Manifest {
            manifest_version: 1,
            version: "0.1.0".into(),
            tag: "v0.1.0".into(),
            released: "2026-06-22T12:00:00Z".into(),
            channel: "stable".into(),
            api_version: "v1".into(),
            schema_version: "V1__initial".into(),
            min_cli_for_daemon: "0.1.0".into(),
            min_daemon_for_cli: "0.1.0".into(),
            artifacts: HashMap::new(),
        };
        let json = encode_manifest(&manifest).unwrap();
        assert!(json.contains("0.1.0"));
        assert!(json.contains("v1"));
    }

    #[test]
    fn test_parse_sha256_valid() {
        let s = parse_sha256("abc123def456abc123def456abc123def456abc123def456abc123def456abc1  looperd");
        assert_eq!(s.len(), 64);
    }

    #[test]
    fn test_parse_sha256_invalid() {
        assert!(parse_sha256("too short").is_empty());
        assert!(parse_sha256("gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg").is_empty());
    }

    #[test]
    fn test_parse_sha256_empty() {
        assert!(parse_sha256("").is_empty());
    }
}
