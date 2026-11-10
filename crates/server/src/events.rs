//! The live channel: a WebSocket per open table screen. When anything at a table changes,
//! everyone there hears "change" and fetches the table again; a ping is passed on as it is.
//! (A Cloudflare quick tunnel holds back server-sent events, which the website uses, so the
//! host talks WebSocket.)

use crate::access::{self, Credentials};
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

// Tunnels close a quiet connection after about a minute and a half.
const KEEP_ALIVE: Duration = Duration::from_secs(25);

#[derive(Clone, Default)]
pub struct Events {
    channels: Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>,
}

impl Events {
    fn channel(&self, table_id: &str) -> broadcast::Sender<String> {
        let mut channels = self.channels.lock().unwrap_or_else(|p| p.into_inner());
        // Channels nobody listens to any more are dropped as new ones are made.
        channels.retain(|_, sender| sender.receiver_count() > 0);
        channels
            .entry(table_id.to_string())
            .or_insert_with(|| broadcast::channel(64).0)
            .clone()
    }

    /// Something at the table changed: everyone there fetches it again.
    pub fn changed(&self, table_id: &str) {
        let _ = self
            .channel(table_id)
            .send(json!({ "type": "change" }).to_string());
    }

    /// A ping on the map, for a moment, not kept.
    pub fn ping(&self, table_id: &str, ping: Value) {
        let _ = self
            .channel(table_id)
            .send(json!({ "type": "ping", "ping": ping }).to_string());
    }

    pub fn listen(&self, table_id: &str) -> broadcast::Receiver<String> {
        self.channel(table_id).subscribe()
    }
}

#[derive(Deserialize)]
pub struct EventsQuery {
    key: Option<String>,
    seat: Option<String>,
}

/// `GET /api/tables/{id}/events`: opens the live channel for someone at the table.
pub async fn open(
    State(state): State<AppState>,
    Path(table_id): Path<String>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response> {
    // WebSockets skip the browser's cross-site check, so the host makes its own: only the
    // paired website's pages may open one. (A program that sends no Origin still needs a key.)
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    if origin.is_some_and(|origin| {
        !state
            .config
            .allowed_origins
            .iter()
            .any(|allowed| allowed == origin)
    }) {
        return Err(AppError::forbidden("This website may not use this host"));
    }
    let credentials = Credentials::from(&headers, query.key, query.seat);
    access::at_table(&state, &table_id, &credentials)?;
    let events = state.events.listen(&table_id);
    Ok(upgrade.on_upgrade(move |socket| stream(socket, events)))
}

async fn stream(socket: WebSocket, mut events: broadcast::Receiver<String>) {
    let (mut out, mut incoming) = socket.split();
    let mut alive = tokio::time::interval(KEEP_ALIVE);
    alive.tick().await;
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(text) => {
                    if out.send(Message::Text(text.into())).await.is_err() { break; }
                }
                // Too far behind to tell what was missed: one "change" covers it all.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let change = json!({ "type": "change" }).to_string();
                    if out.send(Message::Text(change.into())).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = alive.tick() => {
                if out.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
            message = incoming.next() => match message {
                // The browser says nothing on this channel; it only listens.
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}
