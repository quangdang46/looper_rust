//! Version information for the looperd daemon.
//!
//! Values can be overridden at build time via environment variables:
//! - `LOOPER_BUILD_VERSION`
//! - `LOOPER_BUILD_CHANNEL`
//! - `LOOPER_BUILD_GIT_SHA`
//! - `LOOPER_BUILD_TIMESTAMP`

use serde::Serialize;

/// The semantic version string.
pub fn value() -> &'static str {
    option_env!("LOOPER_BUILD_VERSION").unwrap_or("0.0.0-dev")
}

/// Release channel.
pub fn channel() -> &'static str {
    option_env!("LOOPER_BUILD_CHANNEL").unwrap_or("dev")
}

/// API version identifier.
pub fn api_version() -> &'static str {
    "v1"
}

/// Git commit SHA from build time.
pub fn git_commit_sha() -> &'static str {
    option_env!("LOOPER_BUILD_GIT_SHA").unwrap_or("unknown")
}

/// Build timestamp.
pub fn build_timestamp() -> &'static str {
    option_env!("LOOPER_BUILD_TIMESTAMP").unwrap_or("unknown")
}

#[derive(Serialize)]
pub struct VersionMetadata {
    pub version_source: String,
    pub channel: String,
    pub api_version: String,
    pub git_commit_sha: String,
    pub build_timestamp: String,
}

#[derive(Serialize)]
pub struct VersionInfo {
    pub version: String,
    pub metadata: VersionMetadata,
}

impl VersionInfo {
    pub fn current() -> Self {
        let v = value();
        Self {
            version: v.to_string(),
            metadata: VersionMetadata {
                version_source: if v.contains("dev") {
                    "dev".into()
                } else {
                    "release".into()
                },
                channel: channel().to_string(),
                api_version: api_version().to_string(),
                git_commit_sha: git_commit_sha().to_string(),
                build_timestamp: build_timestamp().to_string(),
            },
        }
    }
}
