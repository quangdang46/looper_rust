#![allow(clippy::type_complexity)]

// Dependencies used by the binary target (not directly by this library)
#[allow(unused_imports)]
use {clap as _, tracing_subscriber as _};

pub mod client;
pub mod error;
pub mod helpers;
pub mod manager;
pub mod policy;
pub mod state;
pub mod types;

// Server module is behind a feature flag or always compiled
// (we want it available for the binary)
pub mod server;

pub use client::NetworkClient;
pub use error::NetworkError;
pub use helpers::*;
pub use manager::Manager;
pub use policy::*;
pub use types::*;

// Convenience re-export
pub use server::ServerState;
