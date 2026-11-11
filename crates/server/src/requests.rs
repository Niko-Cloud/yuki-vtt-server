//! Asking to join. With the table's "ask before letting people in" on (the default), a player
//! who opens the invite link waits outside: the DM sees who it is, with their character, and
//! lets them in or turns them away. A player keeps the token they were given; it becomes their
//! seat's token once they are in.

use crate::access::{self, Credentials};
use crate::error::{AppError, Result};
use crate::ids;
use crate::snapshot::Snapshot;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

// More waiting than this is someone spamming the invite.
const MOST_WAITING: i64 = 10;
// A request nobody answered in a day is forgotten.
const WAITS_FOR: i64 = 24 * 60 * 60 * 1000;
// As many seats as the table has.
const MOST_SEATS: i64 = 12;

/// Puts a player in the waiting line. The seat id and token are the ones they'll have once in.
pub fn ask(
    db: &Connection,
    table_id: &str,
    seat_id: &str,
    token: &str,
    snapshot: &Snapshot,
) -> Result<()> {
    let now = ids::now();
    db.execute(
        "DELETE FROM join_requests WHERE created_at < ?1",
        [now - WAITS_FOR],
    )?;
    let waiting: i64 = db.query_row(
        "SELECT COUNT(*) FROM join_requests WHERE table_id = ?1",
        [table_id],
        |row| row.get(0),
    )?;
    if waiting >= MOST_WAITING {
        return Err(AppError::forbidden(
            "Too many people are waiting to join: try again later",
        ));
    }
    // One request per player: asking again replaces the old one.
    if let Some(player) = &snapshot.player_id {
        db.execute(
            "DELETE FROM join_requests WHERE table_id = ?1 AND player_id = ?2",
            params![table_id, player],
        )?;
    }
    db.execute(
        "INSERT INTO join_requests (id, table_id, token_hash, player_id, snapshot, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![seat_id, table_id, ids::hash(token), snapshot.player_id, serde_json::to_string(snapshot)?, now],
    )?;
    Ok(())
}

/// Who is waiting at a table, oldest first, as the DM sees them.
pub fn waiting(db: &Connection, table_id: &str) -> Result<Vec<Value>> {
    let mut rows = db.prepare("SELECT id, snapshot, created_at FROM join_requests WHERE table_id = ?1 AND created_at >= ?2 ORDER BY created_at")?;
    let found = rows
        .query_map(params![table_id, ids::now() - WAITS_FOR], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    found
        .into_iter()
        .map(|(id, text, at)| {
            let snapshot: Snapshot = serde_json::from_str(&text)?;
            Ok(json!({
                "id": id,
                "name": snapshot.summary.name,
                "classes": snapshot.summary.classes,
                // A guest has no account to tell them apart by.
                "account": snapshot.player_id.is_some(),
                "at": ids::iso(at),
            }))
        })
        .collect()
}

/// Every table's waiting line, for the window app.
pub fn all_waiting(db: &Connection) -> Result<Vec<Value>> {
    let mut rows = db.prepare("SELECT DISTINCT r.table_id, t.name FROM join_requests r JOIN tables t ON t.id = r.table_id")?;
    let tables = rows
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut all = Vec::new();
    for (table_id, name) in tables {
        for mut request in waiting(db, &table_id)? {
            request["tableId"] = json!(table_id);
            request["table"] = json!(name);
            all.push(request);
        }
    }
    Ok(all)
}

/// Lets a waiting player in, or turns them away. Returns false when nobody was waiting.
pub fn answer(state: &AppState, table_id: &str, request_id: &str, accept: bool) -> Result<bool> {
    {
        let db = state.db();
        let found: Option<(String, Option<String>, String, i64)> = db
            .query_row(
                "SELECT token_hash, player_id, snapshot, created_at FROM join_requests WHERE table_id = ?1 AND id = ?2",
                [table_id, request_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((token_hash, player_id, snapshot, _)) = found else {
            return Ok(false);
        };
        if accept {
            let seats: i64 = db.query_row(
                "SELECT COUNT(*) FROM seats WHERE table_id = ?1",
                [table_id],
                |row| row.get(0),
            )?;
            if seats >= MOST_SEATS {
                return Err(AppError::forbidden(format!(
                    "The party is full ({MOST_SEATS} seats)"
                )));
            }
            let now = ids::now();
            db.execute(
                "INSERT INTO seats (id, table_id, token_hash, player_id, snapshot, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![request_id, table_id, token_hash, player_id, snapshot, now],
            )?;
        }
        db.execute(
            "DELETE FROM join_requests WHERE table_id = ?1 AND id = ?2",
            [table_id, request_id],
        )?;
    }
    state.events.changed(table_id);
    Ok(true)
}

/// `POST /api/tables/{id}/requests/{request}/accept` (or `/reject`).
pub async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, request, choice)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &Credentials::from(&headers, None))?;
    let accept = match choice.as_str() {
        "accept" => true,
        "reject" => false,
        _ => return Err(AppError::not_found("Accept or reject")),
    };
    if !answer(&state, &id, &request, accept)? {
        return Err(AppError::not_found("Nobody is waiting with that request"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskSetting {
    ask_to_join: bool,
}

/// `PATCH /api/tables/{id}/ask`: whether newcomers wait for the DM, or sit down at once.
pub async fn set_asking(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AskSetting>,
) -> Result<StatusCode> {
    access::owner(&state, &Credentials::from(&headers, None))?;
    let changed = state.db().execute(
        "UPDATE tables SET ask_to_join = ?2 WHERE id = ?1",
        params![id, body.ask_to_join],
    )?;
    if changed == 0 {
        return Err(AppError::not_found("No such table"));
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/seats/{token}/status`: whether a player is in yet. Turned away (or removed),
/// the token is unknown.
pub async fn status(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<Value>> {
    let db = state.db();
    let hash = ids::hash(&token);
    let seated: i64 = db.query_row(
        "SELECT COUNT(*) FROM seats WHERE token_hash = ?1",
        [&hash],
        |row| row.get(0),
    )?;
    if seated > 0 {
        return Ok(Json(json!({ "status": "seated" })));
    }
    let waiting: i64 = db.query_row(
        "SELECT COUNT(*) FROM join_requests WHERE token_hash = ?1 AND created_at >= ?2",
        params![hash, ids::now() - WAITS_FOR],
        |row| row.get(0),
    )?;
    if waiting > 0 {
        return Ok(Json(json!({ "status": "waiting" })));
    }
    Err(AppError::not_found("The DM didn't let you in"))
}
