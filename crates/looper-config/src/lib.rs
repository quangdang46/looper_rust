#![allow(clippy::type_complexity)]
//! Config loading, validation, merge, and disclosure stamping.
//!
//! 3-layer merge pipeline: **defaults → file → env → CLI**.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use looper_config::loader::ConfigLoader;
//!
//! let config = ConfigLoader::new()
//!     .load()
//!     .expect("valid config");
//!
//! println!("server port: {}", config.server.unwrap().port);
//! ```
//!
//! # Architecture
//!
//! - [`types`] — Full (resolved) config structs with built-in defaults.
//! - [`partial`] — Partial config with all-`Option` fields for the merge
//!   pipeline.
//! - [`loader`] — [`ConfigLoader`] builder that discovers, parses, merges,
//!   and validates.
//! - [`enums`] — Config-specific string enums.
//! - [`validate`] — Validation rules that return a [`ConfigValidation`].
//! - [`disclosure`] — Disclosure-stamp generation and stripping.
//! - [`env`] — `LOOPER_*` environment variable bindings.
//! - [`defaults`] — Default path discovery via `directories::ProjectDirs`.
//! - [`error`] — [`ConfigError`] and [`ConfigValidation`] types.

pub mod defaults;
pub mod disclosure;
pub mod enums;
pub mod env;
pub mod error;
pub mod loader;
pub mod partial;
pub mod permissions;
pub mod types;
pub mod validate;

// Re-exports for convenience.
pub use error::{ConfigError, ConfigValidation, ValidationIssue};
pub use loader::ConfigLoader;
pub use types::Config;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crate_has_main_types() {
        // Verify the primary types are accessible
        let _config = Config::default();
        let _error = ConfigError::Other("test".into());
        let _validation = ConfigValidation::new();
    }
}
