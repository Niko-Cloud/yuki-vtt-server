//! The table while playing: what everyone sees, the log of rolls and chat, rolls made here so
//! nobody can fake one, and the fight's initiative order.

use crate::access::{self, Access, Credentials};
use crate::error::{AppError, Result};
use crate::ids;
use crate::snapshot::Snapshot;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use rand::Rng;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use yuki_vtt_rules::encounter::{self, Combatant, Encounter};
use yuki_vtt_rules::{Advantage, parse_dice, roll_detail};

// What each screen gets of the log; older entries are dropped from the table.
const LOG_SHOWN: usize = 100;
const LOG_KEPT: i64 = 300;

fn d20() -> i32 {
    rand::rng().random_range(1..=20)
}

fn credentials(headers: &HeaderMap) -> Credentials {
    Credentials::from(headers, None, None)
}

struct Seat {
    id: String,
    permissions: Value,
    snapshot: Snapshot,
}

fn seats(db: &Connection, table_id: &str) -> Result<Vec<Seat>> {
    let mut rows = db.prepare(
        "SELECT id, permissions, snapshot FROM seats WHERE table_id = ?1 ORDER BY created_at",
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
    #[allow(dead_code)]
    scene: Option<String>,
}

/// `GET /api/tables/{id}`: the table as this person sees it.
pub async fn view(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(_query): Query<ViewQuery>,
) -> Result<Json<Value>> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    let db = state.db();
    let (name, edition, encounter, handout, active_scene): (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = db.query_row(
        "SELECT name, edition, encounter, handout, active_scene_id FROM tables WHERE id = ?1",
        [&id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let seated = seats(&db, &id)?;
    let characters: Vec<Value> = seated
        .iter()
        .filter(|seat| access.seat_ids.contains(&seat.id))
        .map(|seat| json!({ "seatId": seat.id, "characterId": seat.snapshot.character_id }))
        .collect();
    Ok(Json(json!({
        "party": { "id": id, "name": name, "edition": edition },
        "you": { "role": access.role, "name": access.name, "seatIds": access.seat_ids, "permissions": access.permissions, "characters": characters },
        // Player characters' health is everyone's to see, as at a real table.
        "members": seated.iter().map(|seat| seat.snapshot.member(&seat.id, &seat.permissions)).collect::<Vec<_>>(),
        "encounter": encounter.map(|text| serde_json::from_str::<Value>(&text)).transpose()?,
        "scene": Value::Null,
        "scenes": [],
        "activeSceneId": active_scene,
        "assets": [],
        "handout": handout.map(|text| serde_json::from_str::<Value>(&text)).transpose()?,
        "log": log_for(&db, &id, &access)?,
    })))
}

// The newest entries this person may see, oldest first. A player sees what is for everyone,
// and private rolls meant for their seat.
fn log_for(db: &Connection, table_id: &str, access: &Access) -> Result<Vec<Value>> {
    let mut rows = db.prepare("SELECT id, kind, author, data, visible_to, created_at FROM log WHERE table_id = ?1 ORDER BY seq DESC")?;
    let entries = rows
        .query_map([table_id], |row| {
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
    for (entry, kind, author, data, visible_to, at) in entries {
        let visible_to: Vec<String> = serde_json::from_str(&visible_to)?;
        let mine = visible_to.is_empty()
            || access.runs()
            || visible_to.iter().any(|v| access.seat_ids.contains(v));
        if !mine {
            continue;
        }
        shown.push(json!({
            "id": entry,
            "kind": kind,
            "author": author,
            "data": serde_json::from_str::<Value>(&data)?,
            "hidden": !visible_to.is_empty(),
            "at": ids::iso(at),
        }));
        if shown.len() == LOG_SHOWN {
            break;
        }
    }
    shown.reverse();
    Ok(shown)
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
}

/// `POST /api/tables/{id}/log`: a chat line, or a roll made on the device.
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
            } => {
                let author = author(&db, &access, &id, seat_id.as_deref())?;
                log(
                    &db,
                    &id,
                    "chat",
                    &author,
                    json!({ "text": text(&line, 500, "A message")? }),
                    &[],
                )?;
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
        }
    }
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
        let combatant = Combatant {
            id: format!("npc-{}", ids::id()),
            name: name.clone(),
            initiative,
            bonus: body.bonus,
            seat_id: None,
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

#[derive(Deserialize)]
pub struct SetInitiative {
    initiative: i32,
}

pub async fn set_initiative(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, combatant)): Path<(String, String)>,
    Json(body): Json<SetInitiative>,
) -> Result<StatusCode> {
    fight(&state, &headers, &id, |db| {
        if !(-20..=60).contains(&body.initiative) {
            return Err(AppError::bad("Initiative is -20 to 60"));
        }
        save(
            db,
            &id,
            Some(&encounter::set_initiative(
                &running(db, &id)?,
                &combatant,
                body.initiative,
            )),
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
