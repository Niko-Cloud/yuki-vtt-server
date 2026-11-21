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

    /// A map's picture for screens to fetch now, so it is there the moment the DM shows it.
    pub fn preload(&self, table_id: &str, preload: Value) {
        let _ = self
            .channel(table_id)
            .send(json!({ "type": "preload", "preload": preload }).to_string());
    }

    /// A ruler or pointer trail on the map, as it goes, not kept.
    pub fn mark(&self, table_id: &str, mark: Value) {
        let _ = self
            .channel(table_id)
            .send(json!({ "type": "mark", "mark": mark }).to_string());
    }

    /// A token being dragged, as it goes: for those who run the table, and the players of the
    /// seats given (those who would see it there).
    pub fn mark_for(&self, table_id: &str, mark: Value, seats: &[String]) {
        let text = json!({ "type": "mark", "mark": mark }).to_string();
        let seats = seats.join(",");
        let _ = self
            .channel(table_id)
            .send(format!("{FOR_SOME}{seats}{FOR_SOME}{text}"));
    }

    /// The DM's view while "Follow me" is on, for players' maps to follow, not kept.
    pub fn view(&self, table_id: &str, view: Value) {
        let _ = self
            .channel(table_id)
            .send(json!({ "type": "view", "view": view }).to_string());
    }

    /// Someone pressed the X-card: every screen shows it at once, with no name.
    pub fn xcard(&self, table_id: &str) {
        let _ = self
            .channel(table_id)
            .send(json!({ "type": "xcard" }).to_string());
    }

    /// How many screens have a table open right now, over all tables.
    pub fn listeners(&self) -> usize {
        let channels = self.channels.lock().unwrap_or_else(|p| p.into_inner());
        channels
            .values()
            .map(|sender| sender.receiver_count())
            .sum()
    }

    pub fn listen(&self, table_id: &str) -> broadcast::Receiver<String> {
        self.channel(table_id).subscribe()
    }
}

#[derive(Deserialize)]
pub struct EventsQuery {
    pass: Option<String>,
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
    // paired website's pages may open one. (A program that sends no Origin still needs a pass or a seat.)
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
    let credentials = Credentials::from(&headers, query.pass);
    let access = access::at_table(&state, &table_id, &credentials)?;
    let (runs, seats) = (access.runs(), access.seat_ids);
    let events = state.events.listen(&table_id);
    Ok(upgrade.on_upgrade(move |socket| stream(socket, events, runs, seats)))
}

// Around the seats an event is for (whoever runs the table gets it too), before the event;
// never sent on as it is.
const FOR_SOME: char = '\u{1}';

// The event as this person gets it, or none when it isn't for them.
fn for_me(text: String, runs: bool, mine: &[String]) -> Option<String> {
    let Some(rest) = text.strip_prefix(FOR_SOME) else {
        return Some(text);
    };
    let (seats, event) = rest.split_once(FOR_SOME)?;
    let theirs = runs || seats.split(',').any(|s| mine.iter().any(|m| m == s));
    theirs.then(|| event.to_string())
}

async fn stream(
    socket: WebSocket,
    mut events: broadcast::Receiver<String>,
    runs: bool,
    seats: Vec<String>,
) {
    let (mut out, mut incoming) = socket.split();
    let mut alive = tokio::time::interval(KEEP_ALIVE);
    alive.tick().await;
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(text) => {
                    let Some(text) = for_me(text, runs, &seats) else { continue };
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

#[cfg(test)]
mod for_some_tests {
    use super::*;

    #[test]
    fn sends_an_event_for_some_seats_to_them_and_whoever_runs_the_table_only() {
        let event = format!("{FOR_SOME}s1,s2{FOR_SOME}{{\"type\":\"mark\"}}");
        let mine = |seats: &[&str]| seats.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let plain = Some("{\"type\":\"mark\"}".to_string());
        assert_eq!(for_me(event.clone(), true, &[]), plain);
        assert_eq!(for_me(event.clone(), false, &mine(&["s2"])), plain);
        assert_eq!(for_me(event.clone(), false, &mine(&["s3"])), None);
        // For no seats: whoever runs the table only.
        let none = format!("{FOR_SOME}{FOR_SOME}{{}}");
        assert_eq!(for_me(none.clone(), false, &mine(&["s1"])), None);
        assert_eq!(for_me(none, true, &[]), Some("{}".to_string()));
        // Any other event goes to everyone as it is.
        assert_eq!(for_me("{}".into(), false, &[]), Some("{}".to_string()));
    }
}
