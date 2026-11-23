//! The player's side: the invite, sitting down with a character, keeping it up to date, and
//! leaving. A seat is held with the seat token handed out when joining; the host keeps only
//! its hash.

use crate::error::{AppError, Result};
use crate::ids;
use crate::requests;
use crate::snapshot::{self, Signed, Snapshot};
use crate::state::AppState;
use axum::Json;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Map, Value, json};

// As many as at the website's own tables.
const MOST_SEATS: i64 = 12;

struct Invited {
    id: String,
    name: String,
    edition: String,
    dm_name: String,
    locked: bool,
    ask_to_join: bool,
    guests: bool,
    banned: Vec<crate::tables::Banned>,
}

fn invited(db: &Connection, invite: &str) -> Result<Invited> {
    db.query_row(
        "SELECT id, name, edition, dm_name, locked, banned, ask_to_join, guests FROM tables WHERE invite_token = ?1",
        [invite],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get::<_, String>(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        },
    )
    .optional()?
    .map(
        |(id, name, edition, dm_name, locked, banned, ask_to_join, guests)| -> Result<Invited> {
            Ok(Invited {
                id,
                name,
                edition,
                dm_name,
                locked,
                ask_to_join,
                guests,
                banned: crate::tables::bans(&banned)?,
            })
        },
    )
    .transpose()?
    .ok_or_else(|| {
        AppError::not_found("This invite link doesn't work any more: ask the DM for a new one")
    })
}

/// `GET /api/join/{invite}`: which table the link is for, before picking a character.
pub async fn invite(
    State(state): State<AppState>,
    Path(invite): Path<String>,
) -> Result<Json<Value>> {
    let db = state.db();
    let table = invited(&db, &invite)?;
    let seats: i64 = db.query_row(
        "SELECT COUNT(*) FROM seats WHERE table_id = ?1",
        [&table.id],
        |row| row.get(0),
    )?;
    Ok(Json(json!({
        "party": { "id": table.id, "name": table.name, "edition": table.edition, "dm": table.dm_name, "locked": table.locked, "guests": table.guests },
        "seats": seats,
        "full": seats >= MOST_SEATS,
    })))
}

/// `POST /api/join/{invite}`: sits down with a character the website signed.
pub async fn join(
    State(state): State<AppState>,
    Path(invite): Path<String>,
    Json(signed): Json<Signed>,
) -> Result<(StatusCode, Json<Value>)> {
    let secret = state.config.website_secret.as_deref();
    let snapshot = signed.open(secret)?;
    let (table_id, seat_id, token) = {
        let db = state.db();
        let table = invited(&db, &invite)?;
        if secret.is_some() {
            snapshot.check_join(&table.id, ids::now())?;
            once(&db, &signed.signature)?;
        }
        if table.locked {
            return Err(AppError::forbidden(
                "The DM locked this table: ask them to open it",
            ));
        }
        if !table.guests && snapshot.player_id.is_none() {
            return Err(AppError::forbidden(
                "This table takes players signed in with Discord: sign in, then use the invite again",
            ));
        }
        if snapshot
            .player_id
            .as_ref()
            .is_some_and(|player| table.banned.iter().any(|b| &b.id == player))
        {
            return Err(AppError::forbidden("The DM removed you from this table"));
        }
        let seats: i64 = db.query_row(
            "SELECT COUNT(*) FROM seats WHERE table_id = ?1",
            [&table.id],
            |row| row.get(0),
        )?;
        if seats >= MOST_SEATS {
            return Err(AppError::forbidden(format!(
                "The party is full ({MOST_SEATS} seats)"
            )));
        }
        let (seat_id, token, now) = (ids::id(), ids::secret(), ids::now());
        // The DM lets each newcomer in by hand: the player waits with their token, and only
        // gets a seat once the DM says yes.
        if table.ask_to_join {
            requests::ask(&db, &table.id, &seat_id, &token, &snapshot)?;
            drop(db);
            state.events.changed(&table.id);
            return Ok((
                StatusCode::CREATED,
                Json(
                    json!({ "partyId": table.id, "seatId": seat_id, "seatToken": token, "waiting": true }),
                ),
            ));
        }
        db.execute(
            "INSERT INTO seats (id, table_id, token_hash, player_id, snapshot, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![seat_id, table.id, ids::hash(&token), snapshot.player_id, serde_json::to_string(&snapshot)?, now],
        )?;
        (table.id, seat_id, token)
    };
    state.events.changed(&table_id);
    Ok((
        StatusCode::CREATED,
        Json(json!({ "partyId": table_id, "seatId": seat_id, "seatToken": token })),
    ))
}

/// `POST /api/join/{invite}/screen`: a TV screen sits down with no character, a tool-less map
/// that follows the DM. Only this browser gets the seat token, to leave again.
pub async fn join_screen(
    State(state): State<AppState>,
    Path(invite): Path<String>,
) -> Result<(StatusCode, Json<Value>)> {
    let (table_id, seat_id, token) = {
        let db = state.db();
        let table = invited(&db, &invite)?;
        if table.locked {
            return Err(AppError::forbidden(
                "The DM locked this table: ask them to open it",
            ));
        }
        let seats: i64 = db.query_row(
            "SELECT COUNT(*) FROM seats WHERE table_id = ?1",
            [&table.id],
            |row| row.get(0),
        )?;
        if seats >= MOST_SEATS {
            return Err(AppError::forbidden(format!(
                "The party is full ({MOST_SEATS} seats)"
            )));
        }
        let (seat_id, token, now) = (ids::id(), ids::secret(), ids::now());
        // A screen keeps no sheet: just a name for the DM's list, and no rights on the map.
        db.execute(
            "INSERT INTO seats (id, table_id, token_hash, player_id, snapshot, kind, permissions, created_at, updated_at) VALUES (?1, ?2, ?3, NULL, ?4, 'screen', '[]', ?5, ?5)",
            params![seat_id, table.id, ids::hash(&token), r#"{"summary":{"name":"Screen"}}"#, now],
        )?;
        (table.id, seat_id, token)
    };
    state.events.changed(&table_id);
    Ok((
        StatusCode::CREATED,
        Json(json!({ "partyId": table_id, "seatId": seat_id, "seatToken": token })),
    ))
}

// A signed copy for joining sits down once. The ones kept are only as old as a copy can be
// and still be fresh.
fn once(db: &Connection, signature: &str) -> Result<()> {
    let now = ids::now();
    db.execute(
        "DELETE FROM used_joins WHERE used_at < ?1",
        [now - 30 * 60_000],
    )?;
    let fresh = db.execute(
        "INSERT OR IGNORE INTO used_joins (signature, used_at) VALUES (?1, ?2)",
        params![signature, now],
    )?;
    if fresh == 0 {
        return Err(AppError::conflict(
            "That join was used already: open the invite again",
        ));
    }
    Ok(())
}

/// The seat token a player's browser sends, in the `x-seat-token` header. It never goes in
/// the address, which ends up in logs.
pub struct SeatToken(pub String);

impl<S: Send + Sync> FromRequestParts<S> for SeatToken {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self> {
        parts
            .headers
            .get("x-seat-token")
            .and_then(|value| value.to_str().ok())
            .filter(|token| !token.is_empty())
            .map(|token| SeatToken(token.to_string()))
            .ok_or_else(|| AppError::not_found("That seat is gone: the DM may have removed it"))
    }
}

fn by_token(db: &Connection, token: &str) -> Result<(String, String, Snapshot, i64)> {
    let row: Option<(String, String, String, i64)> = db
        .query_row(
            "SELECT id, table_id, snapshot, updated_at FROM seats WHERE token_hash = ?1",
            [ids::hash(token)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let (seat, table, text, updated) =
        row.ok_or_else(|| AppError::not_found("That seat is gone: the DM may have removed it"))?;
    Ok((seat, table, serde_json::from_str(&text)?, updated))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Push {
    #[serde(flatten)]
    signed: Signed,
    /// The play state the sheet last had from the host, so a change the DM made since is kept.
    play_base: Option<Map<String, Value>>,
}

/// `PUT /api/seat`: the player saved their character; the answer is the play state
/// with the DM's changes merged in.
pub async fn push(
    State(state): State<AppState>,
    SeatToken(token): SeatToken,
    Json(body): Json<Push>,
) -> Result<Json<Value>> {
    let secret = state.config.website_secret.as_deref();
    let mut given = body.signed.open(secret)?;
    let (table_id, merged) = {
        let db = state.db();
        let (seat, table, stored, _) = by_token(&db, &token)?;
        if secret.is_some() {
            given.check_save(&seat, &stored, ids::now())?;
        }
        if let Some(base) = &body.play_base {
            given.set_state(snapshot::merge(base, &given.state(), &stored.state()));
        }
        // What the player changed on their sheet, for the DM.
        let noted = yuki_vtt_rules::changes::changes(&stored.facts, &given.facts);
        crate::play::note_changes(&db, &table, &given.summary.name, noted)?;
        db.execute(
            "UPDATE seats SET snapshot = ?2, player_id = COALESCE(?3, player_id), updated_at = ?4 WHERE id = ?1",
            params![seat, serde_json::to_string(&given)?, given.player_id, ids::now()],
        )?;
        (table, given.state())
    };
    state.events.changed(&table_id);
    Ok(Json(json!({ "state": merged })))
}

/// `GET /api/seat`: the seat's play state, for the player's open sheet to pick up
/// the DM's changes.
pub async fn pull(
    State(state): State<AppState>,
    SeatToken(token): SeatToken,
) -> Result<Json<Value>> {
    let (_, _, snapshot, updated) = by_token(&state.db(), &token)?;
    Ok(Json(
        json!({ "state": snapshot.state(), "updatedAt": ids::iso(updated) }),
    ))
}

pub async fn leave(
    State(state): State<AppState>,
    SeatToken(token): SeatToken,
) -> Result<StatusCode> {
    let table_id = {
        let db = state.db();
        let (seat, table, _, _) = by_token(&db, &token)?;
        db.execute("DELETE FROM seats WHERE id = ?1", [&seat])?;
        table
    };
    state.events.changed(&table_id);
    Ok(StatusCode::NO_CONTENT)
}
