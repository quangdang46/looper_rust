use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::{Event, Sse};
use chrono::Utc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::types::Context;

/// How often the SSE endpoint polls for new events.
const POLL_INTERVAL_MS: u64 = 200;

/// Maximum events returned per poll.
const MAX_EVENTS_PER_POLL: usize = 200;

/// Build an SSE event stream scoped to a specific project.
///
/// The stream spawns a background task that polls the event log every
/// 200 ms.  New events are pushed into an unbounded channel that axum
/// streams as SSE.  When the client disconnects the channel receiver is
/// dropped, which causes the next `send` to fail and terminates the poll
/// loop.
pub fn project_event_stream(
    ctx: Arc<Context>,
    project_name: String,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>> + Send + 'static> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(async move {
        let mut last_ts = Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string();
        loop {
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;

            let records = match ctx.state.repos().events.list_since(&last_ts) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "SSE poll error");
                    continue;
                }
            };

            // list_since returns newest-first; reverse for chronological.
            let mut count = 0;
            #[allow(clippy::explicit_counter_loop)]
            for record in records.into_iter().rev() {
                if count >= MAX_EVENTS_PER_POLL {
                    break;
                }

                // Project-name filter: the stored project_id is the project name.
                if record.project_id.as_deref() != Some(&project_name) {
                    continue;
                }

                // Advance the cursor.
                if record.created_at > last_ts {
                    last_ts = record.created_at.clone();
                }

                let payload = serde_json::json!({
                    "id": record.id,
                    "event_type": record.event_type,
                    "project_id": record.project_id,
                    "loop_id": record.loop_id,
                    "run_id": record.run_id,
                    "actor_type": record.actor_type,
                    "actor_id": record.actor_id,
                    "actor_display_name": record.actor_display_name,
                    "payload": record.payload_json,
                    "created_at": record.created_at,
                });

                let data = serde_json::to_string(&payload).unwrap_or_default();
                let event = Event::default()
                    .event("event")
                    .data(data)
                    .id(record.id.clone());

                if tx.send(Ok(event)).is_err() {
                    tracing::debug!("SSE client disconnected, stopping poll");
                    return;
                }

                count += 1;
            }
        }
    });

    Sse::new(UnboundedReceiverStream::new(rx)).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Build an SSE event stream for all projects (no filter).
pub fn global_event_stream(
    ctx: Arc<Context>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>> + Send + 'static> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(async move {
        let mut last_ts = Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string();
        loop {
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;

            let records = match ctx.state.repos().events.list_since(&last_ts) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "SSE global poll error");
                    continue;
                }
            };

            for record in records.into_iter().rev().take(MAX_EVENTS_PER_POLL) {
                if record.created_at > last_ts {
                    last_ts = record.created_at.clone();
                }

                let payload = serde_json::json!({
                    "id": record.id,
                    "event_type": record.event_type,
                    "project_id": record.project_id,
                    "loop_id": record.loop_id,
                    "run_id": record.run_id,
                    "actor_type": record.actor_type,
                    "actor_id": record.actor_id,
                    "actor_display_name": record.actor_display_name,
                    "payload": record.payload_json,
                    "created_at": record.created_at,
                });

                let data = serde_json::to_string(&payload).unwrap_or_default();
                let event = Event::default()
                    .event("event")
                    .data(data)
                    .id(record.id.clone());

                if tx.send(Ok(event)).is_err() {
                    tracing::debug!("SSE global client disconnected");
                    return;
                }
            }
        }
    });

    Sse::new(UnboundedReceiverStream::new(rx)).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}
