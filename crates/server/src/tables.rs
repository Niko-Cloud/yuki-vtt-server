//! The DM's side: making tables, the invite, locking, roles and permissions, bans, and the
//! changes the DM makes to a seated character.

use crate::access::{self, Credentials, PERMISSIONS};
use crate::error::{AppError, Result};
use crate::ids;
use crate::snapshot::Snapshot;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use yuki_vtt_rules::hit_points::{self, TableChange};

// As many tables as the website lets one DM have.
const MOST_TABLES: i64 = 20;

fn credentials(headers: &HeaderMap) -> Credentials {
    Credentials::from(headers, None, None)
}

pub fn name(text: &str, what: &str) -> Result<String> {
    let name = text.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(AppError::bad(format!(
            "{what} needs a name of up to 100 letters"
        )));
    }
    Ok(name.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTable {
    /// The website's own id for the party, so the map room uses the same paths here.
    id: Option<String>,
    name: String,
    #[serde(default = "edition")]
    edition: String,
    /// The DM's name as the website knows it, for the log.
    #[serde(default = "dm")]
    dm_name: String,
}

fn edition() -> String {
    "2014".into()
}
fn dm() -> String {
    "DM".into()
}

/// `POST /api/tables`: a new table, with its invite code.
pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<NewTable>,
) -> Result<(StatusCode, Json<Value>)> {
    access::owner(&state, &credentials(&headers))?;
    let table_name = name(&body.name, "A table")?;
    let dm_name = name(&body.dm_name, "The DM")?;
    if body.edition != "2014" && body.edition != "2024" {
        return Err(AppError::bad("The edition is 2014 or 2024"));
    }
    let db = state.db();
    let count: i64 = db.query_row("SELECT COUNT(*) FROM tables", [], |row| row.get(0))?;
    if count >= MOST_TABLES {
        return Err(AppError::forbidden(format!(
            "This host has {MOST_TABLES} tables already: delete one first"
        )));
    }
    let id = match body.id {
        Some(id)
            if !id.is_empty()
                && id.len() <= 40
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
        {
            id
        }
        Some(_) => {
            return Err(AppError::bad(
                "A table's id is up to 40 letters, digits and dashes",
            ));
        }
        None => ids::id(),
    };
    let taken: i64 = db.query_row("SELECT COUNT(*) FROM tables WHERE id = ?1", [&id], |row| {
        row.get(0)
    })?;
    if taken > 0 {
        return Err(AppError::conflict(
            "This host has a table with that id already",
        ));
    }
    let (invite, now) = (ids::secret(), ids::now());
    db.execute(
        "INSERT INTO tables (id, name, edition, dm_name, invite_token, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![id, table_name, body.edition, dm_name, invite, now],
    )?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "inviteToken": invite })),
    ))
}

/// `GET /api/tables`: the DM's tables on this host.
pub async fn list(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Value>> {
    access::owner(&state, &credentials(&headers))?;
    let db = state.db();
    let mut rows = db.prepare(
        "SELECT t.id, t.name, t.edition, t.invite_token, t.locked, t.updated_at,
                (SELECT COUNT(*) FROM seats s WHERE s.table_id = t.id)
         FROM tables t ORDER BY t.updated_at DESC",
    )?;
    let tables = rows
        .query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "edition": row.get::<_, String>(2)?,
                "inviteToken": row.get::<_, String>(3)?,
                "locked": row.get::<_, bool>(4)?,
                "updatedAt": ids::iso(row.get(5)?),
                "seats": row.get::<_, i64>(6)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(Value::Array(tables)))
}

#[derive(Deserialize)]
pub struct Rename {
    name: String,
}

pub async fn rename(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Rename>,
) -> Result<StatusCode> {
    access::owner(&state, &credentials(&headers))?;
    let table_name = name(&body.name, "A table")?;
    found(state.db().execute(
        "UPDATE tables SET name = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, table_name, ids::now()],
    )?)?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    access::owner(&state, &credentials(&headers))?;
    found(
        state
            .db()
            .execute("DELETE FROM tables WHERE id = ?1", [&id])?,
    )?;
    crate::pictures::forget_table(&state, &id);
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/tables/{id}/invite`: a new invite code; the old link stops working.
pub async fn renew_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    access::owner(&state, &credentials(&headers))?;
    let invite = ids::secret();
    found(state.db().execute(
        "UPDATE tables SET invite_token = ?2 WHERE id = ?1",
        params![id, invite],
    )?)?;
    Ok(Json(json!({ "inviteToken": invite })))
}

#[derive(Deserialize)]
pub struct Lock {
    locked: bool,
}

/// Locked, the invite lets no one new in; those seated stay.
pub async fn lock(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Lock>,
) -> Result<StatusCode> {
    access::owner(&state, &credentials(&headers))?;
    found(state.db().execute(
        "UPDATE tables SET locked = ?2 WHERE id = ?1",
        params![id, body.locked],
    )?)?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SetRole {
    role: String,
}

/// A seat becomes a co-DM, who runs the table with the DM, or a player again.
pub async fn set_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, seat)): Path<(String, String)>,
    Json(body): Json<SetRole>,
) -> Result<StatusCode> {
    access::owner(&state, &credentials(&headers))?;
    if body.role != "player" && body.role != "codm" {
        return Err(AppError::bad("A seat is a player or a co-DM"));
    }
    seat_found(state.db().execute(
        "UPDATE seats SET role = ?3 WHERE table_id = ?1 AND id = ?2",
        params![id, seat, body.role],
    )?)?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SetPermissions {
    permissions: Vec<String>,
}

pub async fn set_permissions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, seat)): Path<(String, String)>,
    Json(body): Json<SetPermissions>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    // Kept in the usual order, each once, and only ones the table knows.
    let permissions: Vec<&str> = PERMISSIONS
        .into_iter()
        .filter(|p| body.permissions.iter().any(|given| given == p))
        .collect();
    if permissions.len() != body.permissions.len() {
        return Err(AppError::bad(format!(
            "Permissions are any of {}",
            PERMISSIONS.join(", ")
        )));
    }
    seat_found(state.db().execute(
        "UPDATE seats SET permissions = ?3 WHERE table_id = ?1 AND id = ?2",
        params![id, seat, serde_json::to_string(&permissions)?],
    )?)?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// The DM takes a seat away. The player can join again unless they are banned too.
pub async fn remove_seat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, seat)): Path<(String, String)>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    seat_found(state.db().execute(
        "DELETE FROM seats WHERE table_id = ?1 AND id = ?2",
        params![id, seat],
    )?)?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// Takes the seat away and keeps its player out, by the id the website gave them.
pub async fn ban(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, seat)): Path<(String, String)>,
) -> Result<StatusCode> {
    access::owner(&state, &credentials(&headers))?;
    {
        let db = state.db();
        let found: Option<(Option<String>, String)> = db
            .query_row(
                "SELECT player_id, snapshot FROM seats WHERE table_id = ?1 AND id = ?2",
                params![id, seat],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (player, snapshot) =
            found.ok_or_else(|| AppError::not_found("That character left the party"))?;
        // The DM sees who is kept out by the character they played.
        let name = serde_json::from_str::<Snapshot>(&snapshot)?.summary.name;
        let player = player.ok_or_else(|| {
            AppError::bad("The website didn't say who plays that seat, so it can only be removed")
        })?;
        let banned: String =
            db.query_row("SELECT banned FROM tables WHERE id = ?1", [&id], |row| {
                row.get(0)
            })?;
        let mut banned = bans(&banned)?;
        if !banned.iter().any(|b| b.id == player) {
            banned.push(Banned { id: player, name });
        }
        db.execute(
            "UPDATE tables SET banned = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(&banned)?],
        )?;
        db.execute(
            "DELETE FROM seats WHERE table_id = ?1 AND id = ?2",
            params![id, seat],
        )?;
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unban(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, player)): Path<(String, String)>,
) -> Result<StatusCode> {
    access::owner(&state, &credentials(&headers))?;
    let db = state.db();
    let banned: String = db
        .query_row("SELECT banned FROM tables WHERE id = ?1", [&id], |row| {
            row.get(0)
        })
        .optional()?
        .ok_or_else(|| AppError::not_found(format!("No table with id {id}")))?;
    let banned: Vec<Banned> = bans(&banned)?
        .into_iter()
        .filter(|b| b.id != player)
        .collect();
    db.execute(
        "UPDATE tables SET banned = ?2 WHERE id = ?1",
        params![id, serde_json::to_string(&banned)?],
    )?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/tables/{id}/seats/{seat}/change`: damage, healing, temporary hit points or
/// conditions from the DM. The player's sheet picks the new play state up from its seat.
pub async fn change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, seat)): Path<(String, String)>,
    Json(change): Json<TableChange>,
) -> Result<Json<Value>> {
    access::runner(&state, &id, &credentials(&headers))?;
    match &change {
        TableChange::Damage { amount }
        | TableChange::Heal { amount }
        | TableChange::Temp { amount }
            if !(1..=999).contains(amount) =>
        {
            return Err(AppError::bad("An amount is from 1 to 999"));
        }
        TableChange::Conditions {
            conditions,
            exhaustion,
        } if conditions.len() > 20 || *exhaustion > 6 => {
            return Err(AppError::bad(
                "Up to 20 conditions, and exhaustion from 0 to 6",
            ));
        }
        _ => {}
    }
    let member = {
        let db = state.db();
        let (text, permissions): (String, String) = db
            .query_row(
                "SELECT snapshot, permissions FROM seats WHERE table_id = ?1 AND id = ?2",
                params![id, seat],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("That character left the party"))?;
        let mut snapshot: Snapshot = serde_json::from_str(&text)?;
        let sheet = snapshot.rules_sheet().ok_or_else(|| {
            AppError::conflict(format!("{} has no sheet yet", snapshot.summary.name))
        })?;
        snapshot.set_state(hit_points::apply(&snapshot.state(), sheet, &change));
        db.execute(
            "UPDATE seats SET snapshot = ?3, updated_at = ?4 WHERE table_id = ?1 AND id = ?2",
            params![id, seat, serde_json::to_string(&snapshot)?, ids::now()],
        )?;
        snapshot.member(&seat, &serde_json::from_str(&permissions)?)
    };
    state.events.changed(&id);
    Ok(Json(member))
}

/// Someone the DM keeps out of a table: the id the website gave them, and the character they
/// played, for the DM's list.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct Banned {
    pub id: String,
    pub name: String,
}

/// A table's bans as stored. Early versions kept the ids alone.
pub fn bans(stored: &str) -> Result<Vec<Banned>> {
    let rows: Vec<Value> = serde_json::from_str(stored)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match row {
            Value::String(id) => Some(Banned {
                id,
                name: "A player".into(),
            }),
            other => serde_json::from_value(other).ok(),
        })
        .collect())
}

/// `GET /api/tables/{id}/party`: the DM view's side of a table: every seated character with
/// the numbers a DM asks for, who is a co-DM, and who is kept out.
pub async fn party(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    let db = state.db();
    let (locked, banned): (bool, String) = db.query_row(
        "SELECT locked, banned FROM tables WHERE id = ?1",
        [&id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut rows =
        db.prepare("SELECT id, role, snapshot FROM seats WHERE table_id = ?1 ORDER BY created_at")?;
    let seats = rows
        .query_map([&id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut members = Vec::new();
    for (seat_id, role, text) in seats {
        let snapshot: Snapshot = serde_json::from_str(&text)?;
        members.push(json!({
            "seatId": seat_id,
            "browser": snapshot.character_id.is_none(),
            "player": Value::Null,
            "role": role,
            // The website said who plays it: they can be made co-DM or kept out.
            "account": snapshot.player_id.is_some(),
            "member": snapshot.dm_member(),
        }));
    }
    Ok(Json(
        json!({ "you": access.role, "locked": locked, "members": members, "banned": bans(&banned)? }),
    ))
}

fn found(changed: usize) -> Result<()> {
    if changed == 0 {
        Err(AppError::not_found("No such table"))
    } else {
        Ok(())
    }
}

fn seat_found(changed: usize) -> Result<()> {
    if changed == 0 {
        Err(AppError::not_found("That character left the party"))
    } else {
        Ok(())
    }
}
