//! The table while playing: what everyone sees, the log of rolls and chat, rolls made here so
//! nobody can fake one, and the fight's initiative order.

use crate::access::{self, Access, Credentials};
use crate::error::{AppError, Result};
use crate::ids;
use crate::maps;
use crate::pictures;
use crate::snapshot::Snapshot;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use rand::Rng;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use yuki_vtt_rules::changes::{self, Change};
use yuki_vtt_rules::encounter::{self, Combatant, Effect, Encounter, Hp};
use yuki_vtt_rules::{Advantage, parse_dice, roll_detail};

// What each screen gets of the log with the table: the newest few, so every download stays
// small. Older entries come a page at a time as the log is scrolled; past LOG_KEPT they go.
const LOG_SHOWN: usize = 30;
const LOG_PAGE: usize = 30;
const LOG_KEPT: i64 = 300;

fn d20() -> i32 {
    rand::rng().random_range(1..=20)
}

fn credentials(headers: &HeaderMap) -> Credentials {
    Credentials::from(headers, None)
}

struct Seat {
    id: String,
    permissions: Value,
    snapshot: Snapshot,
}

fn seats(db: &Connection, table_id: &str) -> Result<Vec<Seat>> {
    let mut rows = db.prepare(
        "SELECT id, permissions, snapshot FROM seats WHERE table_id = ?1 AND kind != 'screen' ORDER BY created_at",
    )?;
    let found = rows
        .query_map([table_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    found
        .into_iter()
        .map(|(id, permissions, snapshot)| {
            Ok(Seat {
                id,
                permissions: serde_json::from_str(&permissions)?,
                snapshot: serde_json::from_str(&snapshot)?,
            })
        })
        .collect()
}

#[derive(Deserialize)]
pub struct ViewQuery {
    // The map the DM is looking at; players always see the one the DM shows.
    scene: Option<String>,
    // The version of the table this screen already has: then only what changed comes back.
    base: Option<String>,
}

/// `GET /api/tables/{id}`: the table as this person sees it.
pub async fn view(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ViewQuery>,
) -> Result<Json<Value>> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    let db = state.db();
    let (name, edition, encounter, handout, active_scene, prefabs): (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = db.query_row(
        "SELECT name, edition, encounter, handout, active_scene_id, prefabs FROM tables WHERE id = ?1",
        [&id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    let timer: Option<String> =
        db.query_row("SELECT timer FROM tables WHERE id = ?1", [&id], |row| {
            row.get(0)
        })?;
    let music: Option<String> =
        db.query_row("SELECT music FROM tables WHERE id = ?1", [&id], |row| {
            row.get(0)
        })?;
    let background: Option<String> = db.query_row(
        "SELECT background FROM tables WHERE id = ?1",
        [&id],
        |row| row.get(0),
    )?;
    let (loot, journal): (Option<String>, Option<String>) = db.query_row(
        "SELECT loot, journal FROM tables WHERE id = ?1",
        [&id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let seated = seats(&db, &id)?;
    let looking = if access.runs() {
        query.scene.clone().or(active_scene.clone())
    } else {
        active_scene.clone()
    };
    let mut shown = match &looking {
        Some(scene) => maps::scene(&db, &id, scene)?,
        None => None,
    };
    // The DM's layer (hidden tokens and drawings, notes) never leaves the host for players.
    if let Some(scene) = shown.as_mut()
        && !access.runs()
    {
        let looking = crate::sight::Looking {
            seat_ids: &access.seat_ids,
            shared: crate::fog::shared_sight(&db, &id)?,
            darkvision: seated
                .iter()
                .map(|seat| {
                    let member = seat.snapshot.member(&seat.id, &seat.permissions);
                    (seat.id.clone(), member["darkvision"].as_f64())
                })
                .collect(),
        };
        crate::fog::for_players(scene, &looking);
    }
    let characters: Vec<Value> = seated
        .iter()
        .filter(|seat| access.seat_ids.contains(&seat.id))
        .map(|seat| json!({ "seatId": seat.id, "characterId": seat.snapshot.character_id }))
        .collect();
    // The screens watching this table, for the DM to see and remove.
    let screens: Vec<Value> = if access.runs() {
        let mut rows = db.prepare(
            "SELECT id FROM seats WHERE table_id = ?1 AND kind = 'screen' ORDER BY created_at",
        )?;
        let ids = rows
            .query_map([&id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|seat_id| json!({ "seatId": seat_id, "name": "Screen" }))
            .collect()
    } else {
        vec![]
    };
    let (log, log_more) = log_page(&db, &id, &access, None, LOG_SHOWN)?;
    let view = json!({
        "party": { "id": id, "name": name, "edition": edition, "sharedSight": crate::fog::shared_sight(&db, &id)? },
        "you": { "role": access.role, "name": access.name, "seatIds": access.seat_ids, "permissions": access.permissions, "screen": access.screen, "characters": characters },
        // Player characters' health is everyone's to see, as at a real table.
        "members": seated.iter().map(|seat| seat.snapshot.member(&seat.id, &seat.permissions)).collect::<Vec<_>>(),
        // Players get the fight without the DM's numbers.
        "encounter": encounter
            .map(|text| serde_json::from_str::<Encounter>(&text))
            .transpose()?
            .map(|fight| if access.runs() { fight } else { encounter::for_players(&fight) }),
        "scene": shown,
        "scenes": if access.runs() { maps::list(&db, &id)? } else { vec![] },
        "activeSceneId": active_scene,
        "assets": pictures::list(&db, &id, access.runs(), shown.as_ref())?,
        "handout": handout.map(|text| serde_json::from_str::<Value>(&text)).transpose()?,
        // The track the DM plays to everyone, looping; null when the table is quiet.
        "music": music.map(|text| serde_json::from_str::<Value>(&text)).transpose()?,
        // The picture behind the map for everyone; null when there is none.
        "background": background.map(|text| serde_json::from_str::<Value>(&text)).transpose()?,
        // The countdown the DM started, for everyone to watch.
        "timer": timer.map(|text| serde_json::from_str::<Value>(&text)).transpose()?,
        // The party's shared loot list and session journal; everyone at the table reads them.
        "loot": loot.map(|text| serde_json::from_str::<Value>(&text)).transpose()?.unwrap_or_else(|| json!([])),
        "journal": journal.map(|text| serde_json::from_str::<Value>(&text)).transpose()?.unwrap_or_else(|| json!([])),
        // Saved groups are the DM's alone; players never receive them.
        "prefabs": if access.runs() { maps::parse_prefabs(prefabs) } else { vec![] },
        // The TV screens watching this table; the DM's alone.
        "screens": screens,
        "log": log,
        "logMore": log_more,
        // Waiting to join: for whoever runs the table, to let in from the map room.
        "requests": if access.runs() { crate::requests::waiting(&db, &id)? } else { vec![] },
    });
    // Who the copy is for: what they may see decides what they get.
    let mut seats = access.seat_ids.clone();
    seats.sort();
    let viewer = format!(
        "{id}|{:?}|{}|{}|{}",
        access.role,
        seats.join(","),
        access.screen,
        query.scene.as_deref().unwrap_or("")
    );
    drop(db);
    Ok(Json(state.versions.answer(
        &viewer,
        query.base.as_deref(),
        view,
    )))
}

// A page of the entries this person may see, oldest first, ending just before `before` (or at
// the newest), and whether older ones are left. A player sees what is for everyone, and
// private rolls meant for their seat.
fn log_page(
    db: &Connection,
    table_id: &str,
    access: &Access,
    before: Option<i64>,
    page: usize,
) -> Result<(Vec<Value>, bool)> {
    let mut rows = db.prepare(
        "SELECT id, kind, author, data, visible_to, created_at FROM log
         WHERE table_id = ?1 AND seq < ?2 ORDER BY seq DESC",
    )?;
    let entries = rows
        .query_map(params![table_id, before.unwrap_or(i64::MAX)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut shown = Vec::new();
    let mut more = false;
    for (entry, kind, author, data, visible_to, at) in entries {
        let visible_to: Vec<String> = serde_json::from_str(&visible_to)?;
        let mine = visible_to.is_empty()
            || access.runs()
            || visible_to.iter().any(|v| access.seat_ids.contains(v));
        if !mine {
            continue;
        }
        if shown.len() == page {
            more = true;
            break;
        }
        shown.push(json!({
            "id": entry,
            "kind": kind,
            "author": author,
            "data": serde_json::from_str::<Value>(&data)?,
            "hidden": !visible_to.is_empty(),
            "at": ids::iso(at),
        }));
    }
    shown.reverse();
    Ok((shown, more))
}

#[derive(Deserialize)]
pub struct Before {
    before: Option<String>,
}

/// `GET /api/tables/{id}/log?before=`: older log entries, a page at a time, as the log is
/// scrolled back.
pub async fn older_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<Before>,
) -> Result<Json<Value>> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    let before = query
        .before
        .ok_or_else(|| AppError::bad("Say which entry to read back from"))?;
    let db = state.db();
    let seq: i64 = db
        .query_row(
            "SELECT seq FROM log WHERE table_id = ?1 AND id = ?2",
            [&id, &before],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("That log entry is gone"))?;
    let (entries, more) = log_page(&db, &id, &access, Some(seq), LOG_PAGE)?;
    Ok(Json(json!({ "entries": entries, "more": more })))
}

/// Who a whisper is for, and who may read it. The DM whispers to one seat; a player only to
/// the DM. Either way whoever runs the table sees it, and the whispering player's own seat.
fn whisper_to(
    db: &Connection,
    access: &Access,
    table_id: &str,
    to: &str,
) -> Result<(String, Vec<String>)> {
    if !access.runs() {
        if to != "dm" {
            return Err(AppError::bad("A player whispers only to the DM"));
        }
        let mut visible_to = vec!["dm".to_string()];
        visible_to.extend(access.seat_ids.iter().cloned());
        return Ok(("the DM".into(), visible_to));
    }
    let text: String = db
        .query_row(
            "SELECT snapshot FROM seats WHERE table_id = ?1 AND id = ?2",
            [table_id, to],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("That character left the party"))?;
    let name = serde_json::from_str::<Snapshot>(&text)?.summary.name;
    Ok((name, vec!["dm".into(), to.to_string()]))
}

// Saves this close together from one player make one note, not a stream of them.
const SAME_CHANGE_MS: i64 = 2 * 60 * 1000;

/// What a player changed on their own sheet, in the log for the DM's eyes only. Added to the
/// player's last note when it is recent, so a run of saves reads as one change.
pub fn note_changes(
    db: &Connection,
    table_id: &str,
    author: &str,
    changes: Vec<Change>,
) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let last: Option<(i64, String, String, String, i64)> = db
        .query_row(
            "SELECT seq, kind, author, data, created_at FROM log WHERE table_id = ?1 ORDER BY seq DESC LIMIT 1",
            [table_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;
    if let Some((seq, kind, by, data, at)) = last
        && kind == "change"
        && by == author
        && at > ids::now() - SAME_CHANGE_MS
    {
        let earlier: Vec<Change> =
            serde_json::from_value(serde_json::from_str::<Value>(&data)?["changes"].clone())?;
        let merged = changes::merge(&earlier, &changes);
        if merged.is_empty() {
            // Changed and changed back: nothing to tell.
            db.execute("DELETE FROM log WHERE seq = ?1", [seq])?;
        } else {
            db.execute(
                "UPDATE log SET data = ?2, created_at = ?3 WHERE seq = ?1",
                params![seq, json!({ "changes": merged }).to_string(), ids::now()],
            )?;
        }
        return Ok(());
    }
    log(
        db,
        table_id,
        "change",
        author,
        json!({ "changes": changes }),
        &["dm".to_string()],
    )
}

/// Writes to the log and keeps only the newest entries: an evening's play, not a campaign's.
pub fn log(
    db: &Connection,
    table_id: &str,
    kind: &str,
    author: &str,
    data: Value,
    visible_to: &[String],
) -> Result<()> {
    let now = ids::now();
    db.execute(
        "INSERT INTO log (id, table_id, kind, author, data, visible_to, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![ids::id(), table_id, kind, author, data.to_string(), serde_json::to_string(visible_to)?, now],
    )?;
    db.execute(
        "DELETE FROM log WHERE table_id = ?1 AND seq <= (SELECT seq FROM log WHERE table_id = ?1 ORDER BY seq DESC LIMIT 1 OFFSET ?2)",
        params![table_id, LOG_KEPT],
    )?;
    db.execute(
        "UPDATE tables SET updated_at = ?2 WHERE id = ?1",
        params![table_id, now],
    )?;
    Ok(())
}

// Who a roll or a line is from: one of your seats' characters, or you.
fn author(
    db: &Connection,
    access: &Access,
    table_id: &str,
    seat_id: Option<&str>,
) -> Result<String> {
    let Some(seat_id) = seat_id else {
        return Ok(access.name.clone());
    };
    if !access.seat_ids.iter().any(|s| s == seat_id) {
        return Err(AppError::not_found("Not your seat"));
    }
    let text: String = db.query_row(
        "SELECT snapshot FROM seats WHERE table_id = ?1 AND id = ?2",
        [table_id, seat_id],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str::<Snapshot>(&text)?.summary.name)
}

fn text(value: &str, most: usize, what: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > most {
        return Err(AppError::bad(format!("{what} is 1 to {most} letters")));
    }
    Ok(trimmed.to_string())
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Post {
    Chat {
        text: String,
        #[serde(rename = "seatId")]
        seat_id: Option<String>,
        /// A whisper: from the DM to one seat, or from a player to the DM ("dm").
        to: Option<String>,
    },
    /// A roll a sheet made on the device, one whose result it needed that instant (a death
    /// save): the log says so.
    Roll {
        label: String,
        expression: String,
        total: i32,
        detail: String,
        #[serde(default)]
        advantage: Advantage,
        #[serde(default)]
        critical: bool,
        #[serde(rename = "seatId")]
        seat_id: Option<String>,
    },
    /// A framed line the DM speaks as a character: a name, an optional portrait, shown big to all.
    Speech {
        name: String,
        text: String,
        #[serde(rename = "assetId")]
        asset_id: Option<String>,
        #[serde(rename = "seatId")]
        seat_id: Option<String>,
    },
}

/// `POST /api/tables/{id}/log`: a chat line, a roll made on the device, or a framed line the DM
/// speaks as a character.
pub async fn post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(post): Json<Post>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    {
        let db = state.db();
        match post {
            Post::Chat {
                text: line,
                seat_id,
                to,
            } => {
                let author = author(&db, &access, &id, seat_id.as_deref())?;
                let line = text(&line, 500, "A message")?;
                match to {
                    None => log(&db, &id, "chat", &author, json!({ "text": line }), &[])?,
                    Some(to) => {
                        let (name, visible_to) = whisper_to(&db, &access, &id, &to)?;
                        log(
                            &db,
                            &id,
                            "chat",
                            &author,
                            json!({ "text": line, "to": name }),
                            &visible_to,
                        )?;
                    }
                }
            }
            Post::Roll {
                label,
                expression,
                total,
                detail,
                advantage,
                critical,
                seat_id,
            } => {
                if !(-1000..=10_000).contains(&total) || expression.len() > 80 || detail.len() > 300
                {
                    return Err(AppError::bad("That roll is out of bounds"));
                }
                let author = author(&db, &access, &id, seat_id.as_deref())?;
                let data = json!({ "label": text(&label, 120, "A roll's name")?, "expression": expression, "total": total, "detail": detail, "advantage": advantage, "critical": critical, "by": "device" });
                log(&db, &id, "roll", &author, data, &[])?;
            }
            Post::Speech {
                name,
                text: line,
                asset_id,
                seat_id,
            } => {
                if !access.runs() {
                    return Err(AppError::forbidden("Only the DM speaks as a character"));
                }
                let author = author(&db, &access, &id, seat_id.as_deref())?;
                let mut data = json!({ "name": text(&name, 60, "A name")?, "text": text(&line, 500, "A message")? });
                if let Some(asset) = asset_id {
                    if asset.is_empty() || asset.len() > 40 {
                        return Err(AppError::bad("A portrait is a picture from the table"));
                    }
                    data["assetId"] = json!(asset);
                }
                log(&db, &id, "speech", &author, data, &[])?;
            }
        }
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/tables/{id}/xcard`: anyone presses the X-card; everyone sees it at once, and no
/// name is shown. The live event raises the banner on every screen; the log keeps an anonymous line.
pub async fn xcard(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    access::at_table(&state, &id, &credentials(&headers))?;
    {
        let db = state.db();
        log(&db, &id, "xcard", "", json!({ "text": "X-card" }), &[])?;
    }
    state.events.xcard(&id);
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimerRequest {
    seconds: i64,
}

/// `PUT /api/tables/{id}/timer`: a countdown everyone watches, ending this many seconds from now.
pub async fn set_timer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<TimerRequest>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    if !(1..=3600).contains(&body.seconds) {
        return Err(AppError::bad(
            "A countdown runs from one second to one hour",
        ));
    }
    let timer = json!({ "endsAt": ids::now() + body.seconds * 1000 }).to_string();
    state
        .db()
        .execute("UPDATE tables SET timer = ?2 WHERE id = ?1", [&id, &timer])?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/tables/{id}/timer`: stops the countdown and takes it off everyone's screen.
pub async fn clear_timer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    state
        .db()
        .execute("UPDATE tables SET timer = NULL WHERE id = ?1", [&id])?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// The party's loot list, checked and tidied: a name, a count from 1 to 999, an optional note.
fn loot_items(value: &Value) -> Result<Vec<Value>> {
    let items = value
        .as_array()
        .ok_or_else(|| AppError::bad("Loot is a list of items"))?;
    if items.len() > 200 {
        return Err(AppError::bad("That is too much loot for one list"));
    }
    items
        .iter()
        .map(|item| {
            let id = item["id"].as_str().unwrap_or("");
            let name = item["name"].as_str().unwrap_or("").trim();
            let quantity = item["quantity"].as_i64().unwrap_or(1);
            let note = item["note"].as_str().unwrap_or("").trim();
            if id.is_empty()
                || id.len() > 40
                || name.is_empty()
                || name.chars().count() > 80
                || !(1..=999).contains(&quantity)
                || note.chars().count() > 200
            {
                return Err(AppError::bad(
                    "A loot item needs a name and a count from 1 to 999",
                ));
            }
            Ok(json!({ "id": id, "name": name, "quantity": quantity, "note": note }))
        })
        .collect()
}

/// The session journal, checked and tidied: a title, optional text, and the moment it was written.
fn journal_entries(value: &Value) -> Result<Vec<Value>> {
    let entries = value
        .as_array()
        .ok_or_else(|| AppError::bad("The journal is a list of entries"))?;
    if entries.len() > 200 {
        return Err(AppError::bad(
            "That is too many journal entries for one table",
        ));
    }
    entries
        .iter()
        .map(|entry| {
            let id = entry["id"].as_str().unwrap_or("");
            let title = entry["title"].as_str().unwrap_or("").trim();
            let text = entry["text"].as_str().unwrap_or("").trim();
            let at = entry["at"]
                .as_i64()
                .ok_or_else(|| AppError::bad("A journal entry needs a time"))?;
            if id.is_empty()
                || id.len() > 40
                || title.is_empty()
                || title.chars().count() > 100
                || text.chars().count() > 2000
            {
                return Err(AppError::bad("A journal entry needs a title"));
            }
            Ok(json!({ "id": id, "title": title, "text": text, "at": at }))
        })
        .collect()
}

/// `PUT /api/tables/{id}/loot`: the party's whole shared loot list; only the DM writes it.
pub async fn set_loot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    let items = loot_items(&body["items"])?;
    let text = serde_json::to_string(&items)?;
    state
        .db()
        .execute("UPDATE tables SET loot = ?2 WHERE id = ?1", [&id, &text])?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/tables/{id}/journal`: the whole session journal, newest first; only the DM writes it.
pub async fn set_journal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    let entries = journal_entries(&body["entries"])?;
    let text = serde_json::to_string(&entries)?;
    state
        .db()
        .execute("UPDATE tables SET journal = ?2 WHERE id = ?1", [&id, &text])?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollRequest {
    label: String,
    expression: String,
    #[serde(default)]
    advantage: Advantage,
    #[serde(default)]
    critical: bool,
    seat_id: Option<String>,
    /// Only the DM sees it, and the player who rolled it.
    #[serde(default)]
    hidden: bool,
}

/// `POST /api/tables/{id}/roll`: the host rolls, logs it, and hands the dice back.
pub async fn roll(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RollRequest>,
) -> Result<Json<yuki_vtt_rules::Roll>> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    let label = text(&request.label, 120, "A roll's name")?;
    let dice = parse_dice(&request.expression)
        .filter(|_| request.expression.len() <= 80)
        .ok_or_else(|| AppError::bad(format!("Can't roll \"{}\"", request.expression)))?;
    let mut random = rand::rng();
    let result = yuki_vtt_rules::roll(&dice, request.advantage, request.critical, |die| {
        random.random_range(1..=die)
    });
    {
        let db = state.db();
        let author = author(&db, &access, &id, request.seat_id.as_deref())?;
        let visible_to: Vec<String> = match (request.hidden, access.runs()) {
            (false, _) => vec![],
            (true, true) => vec!["dm".into()],
            (true, false) => std::iter::once("dm".to_string())
                .chain(access.seat_ids.iter().cloned())
                .collect(),
        };
        let mut data = json!({
            "label": label,
            "expression": request.expression.trim(),
            "total": result.total,
            "detail": roll_detail(&result),
            "advantage": request.advantage,
            "critical": request.critical,
            "by": "server",
        });
        // Who rolled it: a player's character, or the DM.
        if access.runs() && request.seat_id.is_none() {
            data["dm"] = json!(true);
        }
        log(&db, &id, "roll", &author, data, &visible_to)?;
    }
    state.events.changed(&id);
    Ok(Json(result))
}

/// `POST /api/tables/{id}/log/{entry}/reveal`: the DM shows a hidden roll to everyone.
pub async fn reveal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, entry)): Path<(String, String)>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    let changed = state.db().execute(
        "UPDATE log SET visible_to = '[]' WHERE table_id = ?1 AND id = ?2",
        [&id, &entry],
    )?;
    if changed == 0 {
        return Err(AppError::not_found("That roll is gone from the log"));
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/tables/{id}/seats/{seat}`: a party member's sheet, read only, for anyone at the table.
pub async fn seat_sheet(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, seat)): Path<(String, String)>,
) -> Result<Json<Value>> {
    access::at_table(&state, &id, &credentials(&headers))?;
    let text: String = state
        .db()
        .query_row(
            "SELECT snapshot FROM seats WHERE table_id = ?1 AND id = ?2",
            [&id, &seat],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("That character left the party"))?;
    Ok(Json(serde_json::from_str::<Snapshot>(&text)?.sheet_view()))
}

// The fight. Only the DM and co-DMs run it.

fn running(db: &Connection, id: &str) -> Result<Encounter> {
    let stored: Option<String> =
        db.query_row("SELECT encounter FROM tables WHERE id = ?1", [id], |row| {
            row.get(0)
        })?;
    stored
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .ok_or_else(|| AppError::bad("No fight is going on: start one first"))
}

fn save(db: &Connection, id: &str, encounter: Option<&Encounter>) -> Result<()> {
    let text = encounter.map(serde_json::to_string).transpose()?;
    db.execute(
        "UPDATE tables SET encounter = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, text, ids::now()],
    )?;
    Ok(())
}

// Runs a change to the fight for the DM, then tells the table.
fn fight(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
    change: impl FnOnce(&Connection) -> Result<()>,
) -> Result<StatusCode> {
    access::runner(state, id, &credentials(headers))?;
    change(&state.db())?;
    state.events.changed(id);
    Ok(StatusCode::NO_CONTENT)
}

/// Rolls initiative for everyone seated and starts round 1.
pub async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    fight(&state, &headers, &id, |db| {
        let fighters = seats(db, &id)?
            .into_iter()
            .map(|seat| Combatant {
                name: seat.snapshot.summary.name.clone(),
                bonus: seat.snapshot.summary.initiative.unwrap_or(0) as i32,
                initiative: 0,
                seat_id: Some(seat.id.clone()),
                id: seat.id,
                hp: None,
                ac: None,
                health: None,
                effects: vec![],
                moved: None,
            })
            .collect();
        let started = encounter::start(fighters, d20);
        save(db, &id, Some(&started))?;
        let order: Vec<String> = started
            .combatants
            .iter()
            .map(|c| format!("{} {}", c.name, c.initiative))
            .collect();
        log(
            db,
            &id,
            "system",
            "Table",
            json!({ "text": format!("Combat starts. Initiative: {}", order.join(", ")) }),
            &[],
        )
    })
}

#[derive(Deserialize)]
pub struct NewCombatant {
    name: String,
    #[serde(default)]
    bonus: i32,
    initiative: Option<i32>,
    hp: Option<i32>,
    ac: Option<i32>,
}

// Hit points and armour class as the website allows them.
fn hp_ok(hp: Hp) -> Result<Hp> {
    if (1..=99_999).contains(&hp.max) && (0..=99_999).contains(&hp.current) {
        Ok(hp)
    } else {
        Err(AppError::bad(
            "Hit points are 0 to 99,999, the most at least 1",
        ))
    }
}
fn ac_ok(ac: i32) -> Result<i32> {
    if (0..=60).contains(&ac) {
        Ok(ac)
    } else {
        Err(AppError::bad("Armour class is 0 to 60"))
    }
}

pub async fn add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<NewCombatant>,
) -> Result<StatusCode> {
    fight(&state, &headers, &id, |db| {
        let name = text(&body.name, 100, "A name")?;
        if !(-20..=30).contains(&body.bonus)
            || body.initiative.is_some_and(|i| !(-20..=60).contains(&i))
        {
            return Err(AppError::bad(
                "The bonus is -20 to 30 and initiative -20 to 60",
            ));
        }
        let initiative = body.initiative.unwrap_or_else(|| d20() + body.bonus);
        let hp = body
            .hp
            .map(|max| hp_ok(Hp { current: max, max }))
            .transpose()?;
        let combatant = Combatant {
            id: format!("npc-{}", ids::id()),
            name: name.clone(),
            initiative,
            bonus: body.bonus,
            seat_id: None,
            hp,
            ac: body.ac.map(ac_ok).transpose()?,
            health: None,
            effects: vec![],
            moved: None,
        };
        save(
            db,
            &id,
            Some(&encounter::add(&running(db, &id)?, combatant)),
        )?;
        log(
            db,
            &id,
            "system",
            "Table",
            json!({ "text": format!("{name} joins the fight (initiative {initiative}).") }),
            &[],
        )
    })
}

/// A new effect on someone has no id yet.
#[derive(Deserialize)]
pub struct EffectSent {
    id: Option<String>,
    name: String,
    rounds: Option<u32>,
}

/// A change the DM makes to someone in the fight. A null takes hit points or armour class off.
#[derive(Deserialize)]
pub struct CombatantChange {
    name: Option<String>,
    initiative: Option<i32>,
    #[serde(default, deserialize_with = "present")]
    hp: Option<Option<Hp>>,
    #[serde(default, deserialize_with = "present")]
    ac: Option<Option<i32>>,
    effects: Option<Vec<EffectSent>>,
}

// Something sent, even null: Some(None) for null, None when it was left out.
pub(crate) fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    sent: D,
) -> std::result::Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(sent).map(Some)
}

pub async fn change_combatant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, combatant)): Path<(String, String)>,
    Json(body): Json<CombatantChange>,
) -> Result<StatusCode> {
    fight(&state, &headers, &id, |db| {
        let fight = running(db, &id)?;
        if !fight.combatants.iter().any(|c| c.id == combatant) {
            return Err(AppError::not_found("They are not in the fight"));
        }
        if body.initiative.is_some_and(|i| !(-20..=60).contains(&i)) {
            return Err(AppError::bad("Initiative is -20 to 60"));
        }
        let effects = body
            .effects
            .map(|sent| {
                if sent.len() > 20 {
                    return Err(AppError::bad("Someone holds up to 20 effects"));
                }
                sent.into_iter()
                    .map(|e| {
                        if e.rounds.is_some_and(|r| !(1..=10_000).contains(&r)) {
                            return Err(AppError::bad("An effect lasts 1 to 10,000 rounds"));
                        }
                        Ok(Effect {
                            id: e.id.unwrap_or_else(|| format!("fx-{}", ids::id())),
                            name: text(&e.name, 60, "An effect's name")?,
                            rounds: e.rounds,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        let change = encounter::Change {
            name: body
                .name
                .as_deref()
                .map(|n| text(n, 100, "A name"))
                .transpose()?,
            initiative: body.initiative,
            hp: body.hp.map(|hp| hp.map(hp_ok).transpose()).transpose()?,
            ac: body.ac.map(|ac| ac.map(ac_ok).transpose()).transpose()?,
            effects,
        };
        save(
            db,
            &id,
            Some(&encounter::change(&fight, &combatant, change)),
        )
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncounterChange {
    show_health: String,
}

/// What players see of monsters' hit points.
pub async fn change_encounter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<EncounterChange>,
) -> Result<StatusCode> {
    fight(&state, &headers, &id, |db| {
        if !["hidden", "word", "exact"].contains(&body.show_health.as_str()) {
            return Err(AppError::bad("Hit points show as hidden, a word or exact"));
        }
        let fight = Encounter {
            show_health: Some(body.show_health),
            ..running(db, &id)?
        };
        save(db, &id, Some(&fight))
    })
}

#[derive(Deserialize)]
pub struct Reroll {
    who: String,
}

/// Rolls initiative again, for the DM's fighters or for everyone.
pub async fn reroll(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Reroll>,
) -> Result<StatusCode> {
    fight(&state, &headers, &id, |db| {
        let everyone = match body.who.as_str() {
            "all" => true,
            "monsters" => false,
            _ => return Err(AppError::bad("Roll again for monsters or all")),
        };
        let after = encounter::reroll(&running(db, &id)?, everyone, d20);
        save(db, &id, Some(&after))?;
        let order: Vec<String> = after
            .combatants
            .iter()
            .map(|c| format!("{} {}", c.name, c.initiative))
            .collect();
        log(
            db,
            &id,
            "system",
            "Table",
            json!({ "text": format!("Initiative rolled again: {}", order.join(", ")) }),
            &[],
        )
    })
}

pub async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, combatant)): Path<(String, String)>,
) -> Result<StatusCode> {
    fight(&state, &headers, &id, |db| {
        save(
            db,
            &id,
            Some(&encounter::remove(&running(db, &id)?, &combatant)),
        )
    })
}

pub async fn next(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    fight(&state, &headers, &id, |db| {
        let before = running(db, &id)?;
        let after = encounter::next_turn(&before);
        save(db, &id, Some(&after))?;
        let ended: Vec<String> = encounter::effects_ending(&before)
            .into_iter()
            .map(|(who, effect)| format!("{effect} ends on {who}."))
            .collect();
        if !ended.is_empty() {
            log(
                db,
                &id,
                "system",
                "Table",
                json!({ "text": ended.join(" ") }),
                &[],
            )?;
        }
        if after.round > before.round {
            log(
                db,
                &id,
                "system",
                "Table",
                json!({ "text": format!("Round {}.", after.round) }),
                &[],
            )?;
        }
        Ok(())
    })
}

pub async fn previous(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    fight(&state, &headers, &id, |db| {
        save(db, &id, Some(&encounter::previous_turn(&running(db, &id)?)))
    })
}

pub async fn end(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    fight(&state, &headers, &id, |db| {
        let round = running(db, &id)?.round;
        save(db, &id, None)?;
        let rounds = if round == 1 { "round" } else { "rounds" };
        log(
            db,
            &id,
            "system",
            "Table",
            json!({ "text": format!("Combat ends after {round} {rounds}.") }),
            &[],
        )
    })
}
