#![allow(clippy::type_complexity)]
#![doc = "Looper CLI — control and interact with a looper daemon via the REST API."]

// Suppress unused-crate-dependencies lint for deps used only by bin target
use chrono as _;
use reqwest as _;
use serde as _;
use serde_json as _;
use toml as _;
use tracing as _;
use tracing_subscriber as _;
use which as _;

pub mod autoupgrade;
pub mod client;
pub mod commands;
pub mod config_local;
pub mod daemon;
pub mod error;
pub mod output;
pub mod version;
