#![allow(clippy::type_complexity)]
//! Agent executor — spawns and manages AI coding agent processes across 5 vendors.
//!
//! Supports Claude Code, Codex, OpenCode, Cursor CLI, and Hermes.
//! Features:
//! - Per-vendor command resolution and argument construction
//! - Native resume support (except Hermes)
//! - Two-tier timeout (max runtime + heartbeat/idle)
//! - SIGTERM → SIGKILL signal escalation with configurable grace period
//! - Completion marker parsing (`__LOOPER_RESULT__=`)
//! - Native session ID extraction
//! - Agent setup failure detection
//! - Environment variable hardening (unsafe Git env stripping)

#[allow(unused_imports)]
use {looper_config as _, looper_types as _, regex as _, uuid as _};

pub mod args;
pub mod env;
pub mod error;
pub mod executor;
pub mod parse;
pub mod types;

pub use args::{append_completion_instruction, resolve_command, resolve_spawn, resolve_spawn_with_native_resume};
pub use env::{build_command_env, detect_agent_setup_failure};
pub use error::AgentError;
pub use executor::{ConfiguredExecutor, Execution};
pub use parse::{extract_native_session_id, parse_completion};
pub use types::*;
