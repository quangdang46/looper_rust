#![allow(clippy::type_complexity)]

// Dependencies used only transitively (not in this crate's source directly)
#[allow(unused_imports)]
use {looper_config as _, looper_types as _, tokio as _, uuid as _};

pub mod coordinator;
pub mod depgraph;
pub mod dispatch;
pub mod error;
pub mod fixer;
pub mod lifecycle;
pub mod merge_watch;
pub mod middleware;
pub mod permissions;
pub mod planner;
pub mod reviewer;
pub mod reviewer_criteria;
pub mod triage;
pub mod types;
pub mod worker;

pub use coordinator::Coordinator;
pub use dispatch::{decide, needs_dependency_gate, parse_slash_command};
pub use error::{RunnerError, RunnerResult};
pub use fixer::Fixer;
pub use merge_watch::classify_pr;
pub use planner::Planner;
pub use reviewer::Reviewer;
pub use types::*;
pub use worker::Worker;
