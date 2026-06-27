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
pub mod forwarder;
pub mod routing;
pub mod tunnel;
pub mod types;

pub use error::{is_transient_error, WebhookError};
pub use forwarder::{Options as ForwarderOptions, WebhookForwarder};
pub use routing::{is_failing_conclusion, route_event, RoutingDecision};
pub use tunnel::{
    classify_forwarder_exit, derive_tunnel_secret, should_disable_tunnel, ExitClass, GhWebhookClient, GitHubHook,
};
pub use types::{
    DefaultTargetedFixer, DefaultTargetedReviewer, DeliveryRecord, DeliveryRequest, ForwardResult, ForwarderInner,
    Lane, Outcome, Stats, TargetedFixer, TargetedReviewer, WorkItem, WorkKey, WorkMetadata,
};
