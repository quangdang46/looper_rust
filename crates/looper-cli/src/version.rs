/// Fallback if env var is not set at build time.
macro_rules! git_version {
    ($env:tt) => {{
        match option_env!($env) {
            Some(v) => v,
            None => "dev",
        }
    }};
}

/// Build-time version metadata.
pub const VERSION: &str = git_version!("LOOPER_BUILD_VERSION");
pub const CHANNEL: &str = git_version!("LOOPER_BUILD_CHANNEL");
pub const API_VERSION: &str = "0.1.0";
pub const GIT_COMMIT_HASH: &str = git_version!("LOOPER_BUILD_GIT_SHA");
pub const BUILD_TIMESTAMP: &str = git_version!("LOOPER_BUILD_TIMESTAMP");
