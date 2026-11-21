//! Stairs, ladders and portals (the website's map passages): a token that stops on one and
//! goes through comes out at the other end, on this map or another, and its player then sees
//! that map. The far end leads back unless the link was made one way. One asks first unless
//! set to go at once; a hidden one players don't see but fall through; a locked one only the
//! DM sends tokens through.

use crate::access::{self, Access};
use crate::error::{AppError, Result};
use crate::ids;
use crate::maps::{TableMap, Token, active_scene, allow, credentials, follows, load, store};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MOST_PASSAGES: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct End {
    pub map: String,
    pub passage: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Passage {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<End>,
    /// False: a token goes through as soon as it stops there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
}

/// The map someone is on: whoever runs the table, the one shown; a player, the one a passage
/// took their character to, else the one shown.
pub fn scene_for(db: &Connection, access: &Access, table_id: &str) -> Result<Option<String>> {
    if !access.runs() {
        for seat in &access.seat_ids {
            let went: Option<String> = db
                .query_row(
                    "SELECT scene_id FROM seats WHERE table_id = ?1 AND id = ?2",
                    [table_id, seat],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if went.is_some() {
                return Ok(went);
            }
        }
    }
    active_scene(db, table_id)
}

/// Whether a token stands on a passage: its middle within its own half and half a cell.
pub fn on_passage(map: &TableMap, token: &Token, passage: &Passage) -> bool {
    let (x, y) = crate::fog::middle_of(map, token);
    let reach = f64::from(map.grid.size) * (0.5 + token.size / 2.0);
    (x - passage.x).hypot(y - passage.y) <= reach
}

fn name_of(value: &str) -> Result<String> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > 40 {
        return Err(AppError::bad("A passage's name is 1 to 40 letters"));
    }
    Ok(name.to_string())
}

fn on_map(x: f64, y: f64) -> Result<()> {
    if !(0.0..=16_384.0).contains(&x) || !(0.0..=16_384.0).contains(&y) {
        return Err(AppError::bad("A passage goes on the map"));
    }
    Ok(())
}

// Changes a map the DM edits, and tells the table.
fn change_map(
    state: &AppState,
    table_id: &str,
    scene_id: &str,
    change: impl FnOnce(TableMap) -> Result<TableMap>,
) -> Result<()> {
    let db = state.db();
    let map = change(load(&db, table_id, scene_id)?)?;
    store(&db, scene_id, &map)
}

/// `GET /api/tables/{id}/passages`: every passage on the table's maps, for the DM to pick
/// where one leads.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    access::runner(&state, &id, &credentials(&headers))?;
    let db = state.db();
    let mut found = Vec::new();
    for scene in crate::maps::list(&db, &id)? {
        let (Some(map_id), Some(map_name)) = (scene["id"].as_str(), scene["name"].as_str()) else {
            continue;
        };
        for passage in load(&db, &id, map_id)?.passages {
            found.push(json!({ "map": map_id, "mapName": map_name, "id": passage.id, "name": passage.name }));
        }
    }
    Ok(Json(json!(found)))
}

#[derive(Deserialize)]
pub struct NewPassage {
    x: f64,
    y: f64,
    name: Option<String>,
}

pub async fn add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewPassage>,
) -> Result<(StatusCode, Json<Value>)> {
    access::runner(&state, &id, &credentials(&headers))?;
    on_map(body.x, body.y)?;
    let made = format!("pass-{}", ids::id());
    change_map(&state, &id, &scene, |mut map| {
        if map.passages.len() >= MOST_PASSAGES {
            return Err(AppError::bad(format!(
                "A map holds {MOST_PASSAGES} passages"
            )));
        }
        let name = match &body.name {
            Some(name) => name_of(name)?,
            None => format!("Stairs {}", map.passages.len() + 1),
        };
        map.passages.push(Passage {
            id: made.clone(),
            x: body.x,
            y: body.y,
            name,
            to: None,
            ask: None,
            hidden: None,
            locked: None,
        });
        Ok(map)
    })?;
    state.events.changed(&id);
    Ok((StatusCode::CREATED, Json(json!({ "id": made }))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PassageChange {
    x: Option<f64>,
    y: Option<f64>,
    name: Option<String>,
    ask: Option<bool>,
    hidden: Option<bool>,
    locked: Option<bool>,
    /// Where it leads, or null for nowhere.
    #[serde(default, deserialize_with = "crate::maps::present")]
    to: Option<Value>,
    #[serde(default)]
    one_way: bool,
}

// Where the passage at an end leads, changed.
fn lead_to(
    db: &Connection,
    table_id: &str,
    end: &End,
    lead: impl FnOnce(&Passage) -> Option<End>,
) -> Result<()> {
    let Ok(mut map) = load(db, table_id, &end.map) else {
        return Ok(());
    };
    if let Some(passage) = map.passages.iter_mut().find(|p| p.id == end.passage) {
        passage.to = lead(passage);
        store(db, &end.map, &map)?;
    }
    Ok(())
}

/// Changes a passage. Linking it leads it to another (on any of the table's maps), and that one
/// back to it unless one way; unlinking cuts the way back too.
pub async fn change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, passage_id)): Path<(String, String, String)>,
    Json(body): Json<PassageChange>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    let to: Option<Option<End>> = match &body.to {
        None => None,
        Some(Value::Null) => Some(None),
        Some(value) => Some(Some(
            serde_json::from_value::<End>(value.clone())
                .map_err(|_| AppError::bad("A passage leads to a map and a passage"))?,
        )),
    };
    let here = End {
        map: scene.clone(),
        passage: passage_id.clone(),
    };
    {
        let db = state.db();
        if let Some(Some(end)) = &to {
            let leads = end != &here
                && load(&db, &id, &end.map)
                    .is_ok_and(|map| map.passages.iter().any(|p| p.id == end.passage));
            if !leads {
                return Err(AppError::bad("That isn't a passage to lead to"));
            }
        }
        let mut map = load(&db, &id, &scene)?;
        let passage = map
            .passages
            .iter_mut()
            .find(|p| p.id == passage_id)
            .ok_or_else(|| AppError::not_found("That passage is gone"))?;
        let was = passage.to.clone();
        if let (Some(x), Some(y)) = (body.x.or(Some(passage.x)), body.y.or(Some(passage.y))) {
            on_map(x, y)?;
            (passage.x, passage.y) = (x, y);
        }
        if let Some(name) = &body.name {
            passage.name = name_of(name)?;
        }
        if let Some(ask) = body.ask {
            passage.ask = (!ask).then_some(false);
        }
        if let Some(hidden) = body.hidden {
            passage.hidden = hidden.then_some(true);
        }
        if let Some(locked) = body.locked {
            passage.locked = locked.then_some(true);
        }
        if let Some(to) = &to {
            passage.to = to.clone();
        }
        store(&db, &scene, &map)?;
        if let Some(to) = &to {
            // The old far end no longer leads back here.
            if let Some(old) = was.filter(|old| Some(old) != to.as_ref()) {
                lead_to(&db, &id, &old, |p| {
                    p.to.clone().filter(|back| back != &here)
                })?;
            }
            if let Some(end) = to.as_ref().filter(|_| !body.one_way) {
                lead_to(&db, &id, end, |_| Some(here.clone()))?;
            }
        }
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, passage_id)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    {
        let db = state.db();
        let mut map = load(&db, &id, &scene)?;
        let was = map
            .passages
            .iter()
            .find(|p| p.id == passage_id)
            .and_then(|p| p.to.clone());
        map.passages.retain(|p| p.id != passage_id);
        store(&db, &scene, &map)?;
        let here = End {
            map: scene.clone(),
            passage: passage_id,
        };
        if let Some(old) = was {
            lead_to(&db, &id, &old, |p| {
                p.to.clone().filter(|back| back != &here)
            })?;
        }
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// A map went: passages that led there lead nowhere, and players who were on it see the shown
/// map again.
pub fn forget_map(db: &Connection, table_id: &str, gone: &str) -> Result<()> {
    db.execute(
        "UPDATE seats SET scene_id = NULL WHERE table_id = ?1 AND scene_id = ?2",
        [table_id, gone],
    )?;
    for scene in crate::maps::list(db, table_id)? {
        let Some(scene_id) = scene["id"].as_str() else {
            continue;
        };
        let mut map = load(db, table_id, scene_id)?;
        if !map
            .passages
            .iter()
            .any(|p| p.to.as_ref().is_some_and(|to| to.map == gone))
        {
            continue;
        }
        for passage in &mut map.passages {
            if passage.to.as_ref().is_some_and(|to| to.map == gone) {
                passage.to = None;
            }
        }
        store(db, scene_id, &map)?;
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct Through {
    passage: String,
}

/// `POST /api/tables/{id}/scenes/{scene}/tokens/{token}/through`: a token stopped on a passage
/// goes through it.
pub async fn through(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, token_id)): Path<(String, String, String)>,
    Json(body): Json<Through>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    go_through(&state, &access, &id, &scene, &token_id, &body.passage)?;
    Ok(StatusCode::NO_CONTENT)
}

/// A token that stopped on a passage that goes at once (or a hidden one, a trapdoor) goes
/// through; anything else stays.
pub fn fall_through(
    state: &AppState,
    access: &Access,
    table_id: &str,
    scene_id: &str,
    token_id: &str,
) -> Result<()> {
    let trap = {
        let db = state.db();
        let map = load(&db, table_id, scene_id)?;
        let Some(token) = map.tokens.iter().find(|t| t.id == token_id) else {
            return Ok(());
        };
        map.passages
            .iter()
            .find(|p| {
                p.to.is_some()
                    && (p.ask == Some(false) || p.hidden == Some(true))
                    && (p.locked != Some(true) || access.runs())
                    && on_passage(&map, token, p)
            })
            .map(|p| p.id.clone())
    };
    match trap {
        Some(passage) => go_through(state, access, table_id, scene_id, token_id, &passage),
        None => Ok(()),
    }
}

fn go_through(
    state: &AppState,
    access: &Access,
    table_id: &str,
    scene_id: &str,
    token_id: &str,
    passage_id: &str,
) -> Result<()> {
    {
        let db = state.db();
        if !access.runs() && scene_for(&db, access, table_id)?.as_deref() != Some(scene_id) {
            return Err(AppError::not_found("That map is gone"));
        }
        let mut map = load(&db, table_id, scene_id)?;
        let token = map
            .tokens
            .iter()
            .find(|t| t.id == token_id && (access.runs() || t.hidden != Some(true)))
            .cloned()
            .ok_or_else(|| AppError::not_found("That token is gone"))?;
        let mine = token
            .seat_id
            .iter()
            .chain(token.moved_by.iter().flatten())
            .any(|seat| access.seat_ids.contains(seat));
        allow(
            access.may("move-any") || (mine && access.may("move-own")),
            "move it",
        )?;
        let passage = map
            .passages
            .iter()
            .find(|p| p.id == passage_id)
            .cloned()
            .ok_or_else(|| AppError::bad("That passage leads nowhere"))?;
        let to = passage
            .to
            .clone()
            .ok_or_else(|| AppError::bad("That passage leads nowhere"))?;
        if passage.locked == Some(true) && !access.runs() {
            return Err(AppError::bad(format!("{} is locked", passage.name)));
        }
        if !on_passage(&map, &token, &passage) {
            return Err(AppError::bad(format!(
                "{} isn't at {}",
                token.name, passage.name
            )));
        }
        let mut there = if to.map == scene_id {
            map.clone()
        } else {
            load(&db, table_id, &to.map).map_err(|_| AppError::bad("Where it led is gone"))?
        };
        let end = there
            .passages
            .iter()
            .find(|p| p.id == to.passage)
            .cloned()
            .ok_or_else(|| AppError::bad("Where it led is gone"))?;
        // Its middle on the far end, on whole cells (half cells for a tiny token).
        let spot = |on: &TableMap, size: f64| {
            let step = if size < 1.0 { 0.5 } else { 1.0 };
            let cell = |at: f64, offset: u32| {
                (((at - f64::from(offset)) / f64::from(on.grid.size) - size / 2.0) / step).round()
                    * step
            };
            (cell(end.x, on.grid.offset_x), cell(end.y, on.grid.offset_y))
        };
        let riders = crate::maps::carried_by(&map.tokens, token_id);
        let (x, y) = spot(&there, token.size);
        let (dx, dy) = (x - token.x, y - token.y);
        let moved = |t: &Token, on: &TableMap| {
            let mut t = t.clone();
            if t.id == token_id {
                (t.x, t.y) = (x, y);
            } else if follows(&t, "position") {
                (t.x, t.y) = ((t.x + dx).max(0.0), (t.y + dy).max(0.0));
            }
            on.keep_token(t)
        };
        if to.map == scene_id {
            let tokens = map.tokens.clone();
            map.tokens = tokens
                .iter()
                .map(|t| {
                    if t.id == token_id || riders.contains(&t.id) {
                        moved(t, &map)
                    } else {
                        t.clone()
                    }
                })
                .collect();
            store(&db, scene_id, &map)?;
        } else {
            // Onto the other map: off this one with its riders, on there by the far end. A
            // character already on that map is moved there instead of copied.
            let going: Vec<Token> = map
                .tokens
                .iter()
                .filter(|t| t.id == token_id || riders.contains(&t.id))
                .cloned()
                .collect();
            map.tokens.retain(|t| !going.iter().any(|g| g.id == t.id));
            store(&db, scene_id, &map)?;
            let arriving: Vec<Token> = going.iter().map(|t| moved(t, &there)).collect();
            there.tokens.retain(|t| {
                !arriving
                    .iter()
                    .any(|a| a.id == t.id || (a.seat_id.is_some() && a.seat_id == t.seat_id))
            });
            there.tokens.extend(arriving);
            store(&db, &to.map, &there)?;
            // Its player sees that map now.
            if let Some(seat) = &token.seat_id {
                db.execute(
                    "UPDATE seats SET scene_id = ?3 WHERE table_id = ?1 AND id = ?2",
                    params![table_id, seat, to.map],
                )?;
            }
        }
    }
    state.events.changed(table_id);
    Ok(())
}

/// Characters a passage took to another map come back with their players when the DM shows a
/// map: by the passage here that leads where they were, else in the top corner. One already
/// on this map stays where it is.
pub fn bring_back(db: &Connection, table_id: &str, scene_id: &str) -> Result<()> {
    let away: Vec<(String, String)> = db
        .prepare(
            "SELECT id, scene_id FROM seats WHERE table_id = ?1 AND scene_id IS NOT NULL AND scene_id != ?2",
        )?
        .query_map([table_id, scene_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seat, went) in away {
        let (Ok(mut there), Ok(mut here)) =
            (load(db, table_id, &went), load(db, table_id, scene_id))
        else {
            continue;
        };
        let Some(at) = there
            .tokens
            .iter()
            .position(|t| t.seat_id.as_deref() == Some(seat.as_str()))
        else {
            continue;
        };
        if here
            .tokens
            .iter()
            .any(|t| t.seat_id.as_deref() == Some(seat.as_str()))
        {
            continue;
        }
        let mut token = there.tokens.remove(at);
        let (x, y) = match here
            .passages
            .iter()
            .find(|p| p.to.as_ref().is_some_and(|to| to.map == went))
        {
            Some(door) => {
                let cell = |at: f64, offset: u32| {
                    ((at - f64::from(offset)) / f64::from(here.grid.size) - token.size / 2.0)
                        .round()
                        .max(0.0)
                };
                (
                    cell(door.x, here.grid.offset_x),
                    cell(door.y, here.grid.offset_y),
                )
            }
            None => (0.0, 0.0),
        };
        (token.x, token.y) = (x, y);
        here.tokens.push(here.keep_token(token));
        store(db, &went, &there)?;
        store(db, scene_id, &here)?;
    }
    Ok(())
}

/// Where passages took players, seat to map, for whoever runs the table.
pub fn away(db: &Connection, table_id: &str) -> Result<serde_json::Map<String, Value>> {
    let found: Vec<(String, String)> = db
        .prepare("SELECT id, scene_id FROM seats WHERE table_id = ?1 AND scene_id IS NOT NULL")?
        .query_map([table_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(found
        .into_iter()
        .map(|(seat, map)| (seat, json!(map)))
        .collect())
}
