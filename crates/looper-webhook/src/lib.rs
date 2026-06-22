#![allow(clippy::type_complexity)]
//! # looper-webhook
//!
//! GitHub webhook forwarder, event routing, and tunnel management.
//!
//! This crate receives incoming GitHub webhook delivery requests, routes them
//! to the appropriate lanes (reviewer / fixer), deduplicates against in-flight
//! work, and dispatches them to a configurable worker pool. It also provides
//! tunnel health checking helpers.

pub mod error;
pub mod types;
pub mod routing;
pub mod forwarder;
pub mod tunnel;

pub use error::{WebhookError, is_transient_error};
pub use types::{
    DeliveryRequest, DeliveryRecord, ForwardResult, ForwarderInner, Lane, Outcome, Stats,
    TargetedFixer, TargetedReviewer, WorkItem, WorkKey, WorkMetadata,
    DefaultTargetedReviewer, DefaultTargetedFixer,
};
pub use forwarder::{WebhookForwarder, Options as ForwarderOptions};
pub use routing::{route_event, RoutingDecision, is_failing_conclusion};
pub use tunnel::{GhWebhookClient, GitHubHook, derive_tunnel_secret, classify_forwarder_exit, ExitClass, should_disable_tunnel};
