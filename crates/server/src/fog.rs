//! Fog of war: the DM's walls, doors, lights and notes on a map, whether players share the
//! party's sight, and what each player has explored. What each player sees is worked out on
//! their own screen; the host keeps the map and leaves the DM's notes out for players.

use crate::access::{self, Access};
use crate::error::{AppError, Result};
use crate::ids;
use crate::maps::{self, Fog, Light, Note, TableMap, Wall, allow, credentials, edit, not_allowed};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use yuki_vtt_rules::map as grid;

const MOST_WALLS: usize = 2000;
const MOST_LIGHTS: usize = 100;
const MOST_NOTES: usize = 200;
const MOST_FOG_AREAS: usize = 200;
const MOST_EXPLORED_TEXT: usize = 200_000;

/// What the DM sets in one go: the fog, the walls and doors, the lights.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lighting {
    fog: Fog,
    walls: Vec<Wall>,
    lights: Vec<Light>,
    /// Painted fog; left as it is when not sent.
    fog_areas: Option<Vec<maps::FogArea>>,
}

fn on_map(x: f64, y: f64) -> bool {
    x.is_finite() && y.is_finite() && (0.0..=16_384.0).contains(&x) && (0.0..=16_384.0).contains(&y)
}

fn short_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40
}

/// A fog colour is `#rrggbb`, like the website's; none means the default dark.
fn fog_color_ok(color: &Option<String>) -> bool {
    color.as_ref().is_none_or(|c| {
        c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit())
    })
}

impl Lighting {
    fn check(&self) -> Result<()> {
        let fog = ["off", "dynamic"].contains(&self.fog.mode.as_str())
            && ["bright", "dark"].contains(&self.fog.light.as_str())
            && fog_color_ok(&self.fog.color)
            && self
                .fog
                .layer
                .as_deref()
                .is_none_or(|l| l == "single" || l == "stacked");
        let walls = self.walls.len() <= MOST_WALLS
            && self.walls.iter().all(|w| {
                short_id(&w.id)
                    && on_map(w.a[0], w.a[1])
                    && on_map(w.b[0], w.b[1])
                    && w.kind
                        .as_deref()
                        .is_none_or(|k| k == "curtain" || k == "one-way")
            });
        let lights = self.lights.len() <= MOST_LIGHTS
            && self.lights.iter().all(|l| {
                short_id(&l.id)
                    && on_map(l.x, l.y)
                    && l.bright <= 1000
                    && l.dim <= 1000
                    && maps::spread_ok(&l.shape, l.width, &l.edge)
                    && l.angle
                        .is_none_or(|a| a.is_finite() && (-360.0..=360.0).contains(&a))
            });
        let painted = self.fog_areas.as_ref().is_none_or(|areas| {
            areas.len() <= MOST_FOG_AREAS
                && areas.iter().all(|a| {
                    short_id(&a.id)
                        && (3..=400).contains(&a.points.len())
                        && a.points.iter().all(|p| on_map(p[0], p[1]))
                })
        });
        if fog && walls && lights && painted {
            Ok(())
        } else {
            Err(AppError::bad(format!(
                "A map has up to {MOST_WALLS} walls and {MOST_LIGHTS} lights, on the map"
            )))
        }
    }
}

/// `PUT /api/tables/{id}/scenes/{scene}/lighting`: the DM sets the fog, walls and lights.
pub async fn set_lighting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<Lighting>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    body.check()?;
    edit(&state, &access, &id, &scene, |mut map| {
        map.fog = body.fog;
        map.walls = body.walls;
        map.lights = body.lights;
        if let Some(areas) = body.fog_areas {
            map.fog_areas = areas;
        }
        Ok(map)
    })
}

#[derive(Deserialize)]
pub struct DoorChange {
    open: bool,
}

// Where a token's middle is, in pixels.
pub(crate) fn middle_of(map: &TableMap, token: &maps::Token) -> (f64, f64) {
    let g = &map.grid;
    let cell = f64::from(g.size);
    let half = token.size * cell / 2.0;
    (
        f64::from(g.offset_x) + token.x * cell + half,
        f64::from(g.offset_y) + token.y * cell + half,
    )
}

/// Opens or closes a door. The DM may any; a player only one their own token stands next to
/// (within a cell and a half of its middle, from the token's edge).
pub async fn set_door(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, wall)): Path<(String, String, String)>,
    Json(body): Json<DoorChange>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    edit(&state, &access, &id, &scene, |mut map| {
        let at = map
            .walls
            .iter()
            .position(|w| w.id == wall && w.door.is_some())
            .ok_or_else(|| AppError::not_found("That door is gone"))?;
        if !access.runs() {
            let door = &map.walls[at];
            let (mx, my) = ((door.a[0] + door.b[0]) / 2.0, (door.a[1] + door.b[1]) / 2.0);
            let size = f64::from(map.grid.size);
            let near = map.tokens.iter().any(|token| {
                let mine = token
                    .seat_id
                    .as_ref()
                    .is_some_and(|seat| access.seat_ids.contains(seat));
                let (x, y) = middle_of(&map, token);
                let reach = size * (1.5 + token.size / 2.0);
                mine && (x - mx).hypot(y - my) <= reach
            });
            if !near {
                return Err(not_allowed("open a door your token is not next to"));
            }
            // A closed secret door isn't a door at all to players.
            if door.door.as_ref().is_some_and(|d| d.secret && !d.open) {
                return Err(AppError::not_found("That door is gone"));
            }
        }
        // A locked door stays as it is, for the DM too, until the DM unlocks it.
        let door = map.walls[at].door.as_mut().expect("found as a door");
        if door.locked {
            return Err(AppError::bad("That door is locked"));
        }
        door.open = body.open;
        Ok(map)
    })
}

#[derive(Deserialize)]
pub struct NewNote {
    x: f64,
    y: f64,
    text: String,
    #[serde(default)]
    shared: bool,
}

pub async fn add_note(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewNote>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("notes"), "pin notes")?;
    // A player's note is for everyone, and says whose it is.
    let by = if access.runs() {
        None
    } else {
        Some(
            access
                .seat_ids
                .first()
                .cloned()
                .ok_or_else(|| AppError::forbidden("Only a seat pins notes"))?,
        )
    };
    let text = body.text.trim().to_string();
    if text.is_empty() || text.chars().count() > 500 || !on_map(body.x, body.y) {
        return Err(AppError::bad("A note is 1 to 500 letters, on the map"));
    }
    let note = Note {
        id: format!("note-{}", ids::id()),
        x: body.x,
        y: body.y,
        text,
        shared: body.shared || by.is_some(),
        by,
    };
    edit(&state, &access, &id, &scene, |mut map| {
        map.notes.push(note);
        let extra = map.notes.len().saturating_sub(MOST_NOTES);
        map.notes.drain(..extra);
        Ok(map)
    })
}

pub async fn remove_note(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, note)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    edit(&state, &access, &id, &scene, |mut map| {
        // Whoever runs the table takes any off; a player only one they pinned.
        if !access.runs() {
            let theirs = map.notes.iter().any(|n| {
                n.id == note && n.by.as_ref().is_some_and(|by| access.seat_ids.contains(by))
            });
            allow(theirs, "take that note off")?;
        }
        map.notes.retain(|n| n.id != note);
        Ok(map)
    })
}

/// The table's sight rules; only what is sent changes.
#[derive(Deserialize)]
pub struct Sight {
    shared: Option<bool>,
    drag: Option<DragSight>,
}

/// What players see while dragging their token (the website's DragSight): only once it is
/// dropped ("drop"), allies coming into view ("allies"), or all its sight as it goes ("live");
/// live only outside a fight if `outside_fights`, and what is glimpsed explored if `remember`.
#[derive(Deserialize, serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DragSight {
    pub mode: String,
    pub outside_fights: bool,
    pub remember: bool,
}

impl Default for DragSight {
    fn default() -> Self {
        Self {
            mode: "drop".into(),
            outside_fights: true,
            remember: false,
        }
    }
}

/// With fog, whether each player sees through every party token or only their own, and what
/// they see while dragging their token.
pub async fn set_sight(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Sight>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    let db = state.db();
    if let Some(shared) = body.shared {
        db.execute(
            "UPDATE tables SET shared_sight = ?2 WHERE id = ?1",
            params![id, shared],
        )?;
    }
    if let Some(drag) = body.drag {
        if !["drop", "allies", "live"].contains(&drag.mode.as_str()) {
            return Err(AppError::bad(
                "Sight while dragging is drop, allies or live",
            ));
        }
        db.execute(
            "UPDATE tables SET drag_sight = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(&drag)?],
        )?;
    }
    drop(db);
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// The table's rule for sight while dragging (the default when none is set, or it can't be read).
pub fn drag_sight(db: &Connection, table_id: &str) -> Result<DragSight> {
    let stored: Option<String> = db.query_row(
        "SELECT drag_sight FROM tables WHERE id = ?1",
        [table_id],
        |row| row.get(0),
    )?;
    Ok(stored
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default())
}

/// Whether the table shares the party's sight.
pub fn shared_sight(db: &Connection, table_id: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT shared_sight FROM tables WHERE id = ?1",
        [table_id],
        |row| row.get(0),
    )?)
}

/// A map as players get it: without the DM's layer (hidden tokens and drawings, notes).
pub fn for_players(scene: &mut Value, looking: &crate::sight::Looking) {
    if let Some(tokens) = scene["tokens"].as_array_mut() {
        tokens.retain(|t| t["hidden"] != json!(true));
    }
    if let Some(drawings) = scene["drawings"].as_array_mut() {
        drawings.retain(|d| d["hidden"] != json!(true));
    }
    if let Some(templates) = scene["templates"].as_array_mut() {
        templates.retain(|t| t["hidden"] != json!(true));
    }
    // A name the DM keeps off the map isn't sent either, nor a screen-reader description.
    if let Some(tokens) = scene["tokens"].as_array_mut() {
        for t in tokens {
            if t["nameHidden"] == json!(true) {
                t["name"] = json!("Unknown");
            }
            if let Some(object) = t.as_object_mut() {
                object.remove("description");
            }
        }
    }
    // Monsters under the DM's painted fog aren't sent either; party tokens always are.
    if let Ok(map) = serde_json::from_value::<TableMap>(scene.clone())
        && let Some(tokens) = scene["tokens"].as_array_mut()
    {
        tokens.retain(|t| {
            if t.get("seatId").is_some_and(|s| !s.is_null()) {
                return true;
            }
            let Ok(token) = serde_json::from_value::<maps::Token>(t.clone()) else {
                return true;
            };
            !map.hand_fogged(middle_of(&map, &token))
        });
    }
    // With dynamic fog, a token this player can't see right now isn't sent at all: hiding it on
    // screen alone would still let anyone reading the page find it.
    if let Ok(map) = serde_json::from_value::<TableMap>(scene.clone())
        && map.fog.mode == "dynamic"
    {
        let shown = crate::sight::shown_ids(&map, looking);
        if let Some(tokens) = scene["tokens"].as_array_mut() {
            tokens.retain(|t| {
                t["id"]
                    .as_str()
                    .is_some_and(|id| shown.iter().any(|s| s == id))
            });
        }
    }
    // Only notes the DM lets players read.
    if let Some(notes) = scene["notes"].as_array_mut() {
        notes.retain(|n| n["shared"] == json!(true));
    }
    // Hidden passages are the DM's to know of.
    if let Some(passages) = scene["passages"].as_array_mut() {
        passages.retain(|p| p["hidden"] != json!(true));
    }
    // A closed secret door is a wall to players; an open one an open door, its secret kept.
    if let Some(walls) = scene["walls"].as_array_mut() {
        for wall in walls {
            let Some(door) = wall.get_mut("door").and_then(Value::as_object_mut) else {
                continue;
            };
            let open = door.get("open") == Some(&json!(true));
            let secret = door.remove("secret") == Some(json!(true));
            if secret && !open {
                wall.as_object_mut().expect("a wall").remove("door");
            }
        }
    }
}

// Explored cells as text: each row's runs, like "3:0-5,8-9;4:1-2", as the website packs them.
pub(crate) fn unpack(text: &str) -> BTreeSet<(u32, u32)> {
    let mut cells = BTreeSet::new();
    for row in text.split(';') {
        let Some((y, runs)) = row.split_once(':') else {
            continue;
        };
        let Ok(y) = y.parse::<u32>() else { continue };
        for run in runs.split(',') {
            let (from, to) = run.split_once('-').unwrap_or((run, run));
            let (Ok(from), Ok(to)) = (from.parse::<u32>(), to.parse::<u32>()) else {
                continue;
            };
            if to < from || to - from > 2000 {
                continue;
            }
            cells.extend((from..=to).map(|x| (x, y)));
        }
    }
    cells
}

pub(crate) fn pack(cells: &BTreeSet<(u32, u32)>) -> String {
    let mut rows: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &(x, y) in cells {
        rows.entry(y).or_default().push(x);
    }
    rows.into_iter()
        .map(|(y, mut xs)| {
            xs.sort_unstable();
            let mut runs = Vec::new();
            let mut i = 0;
            while i < xs.len() {
                let start = xs[i];
                while i + 1 < xs.len() && xs[i + 1] == xs[i] + 1 {
                    i += 1;
                }
                runs.push(if start == xs[i] {
                    start.to_string()
                } else {
                    format!("{start}-{}", xs[i])
                });
                i += 1;
            }
            format!("{y}:{}", runs.join(","))
        })
        .collect::<Vec<_>>()
        .join(";")
}

// Players reach only the map everyone sees, never the DM's others.
fn reach(db: &Connection, access: &Access, table_id: &str, scene_id: &str) -> Result<TableMap> {
    if !access.runs()
        && crate::passages::scene_for(db, access, table_id)?.as_deref() != Some(scene_id)
    {
        return Err(AppError::not_found("That map is gone"));
    }
    maps::load(db, table_id, scene_id)
}

// Whose explored places a player's screen adds to: their seat's, or the party's.
fn explorer(db: &Connection, access: &Access, table_id: &str) -> Result<String> {
    Ok(if shared_sight(db, table_id)? {
        "party".into()
    } else {
        access
            .seat_ids
            .first()
            .cloned()
            .unwrap_or_else(|| "party".into())
    })
}

fn explored_cells(db: &Connection, scene_id: &str, viewer: &str) -> Result<String> {
    Ok(db
        .query_row(
            "SELECT cells FROM explored WHERE scene_id = ?1 AND viewer = ?2",
            [scene_id, viewer],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default())
}

#[derive(Deserialize)]
pub struct ViewerQuery {
    viewer: Option<String>,
}

/// What a player has seen of a map. The DM may read any viewer's (seeing as them).
pub async fn explored(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Query(query): Query<ViewerQuery>,
) -> Result<Json<Value>> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    let db = state.db();
    reach(&db, &access, &id, &scene)?;
    let viewer = match query.viewer {
        Some(viewer) if access.runs() => viewer,
        _ => explorer(&db, &access, &id)?,
    };
    Ok(Json(
        json!({ "cells": explored_cells(&db, &scene, &viewer)? }),
    ))
}

#[derive(Deserialize)]
pub struct Explored {
    cells: String,
}

/// Adds what a player's screen saw, when the map remembers it; cells off the map are dropped.
pub async fn explore(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<Explored>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    if body.cells.len() > MOST_EXPLORED_TEXT {
        return Err(AppError::bad("That's more than a map holds"));
    }
    if access.runs() {
        return Ok(StatusCode::NO_CONTENT);
    }
    let db = state.db();
    let map = reach(&db, &access, &id, &scene)?;
    if map.fog.mode != "dynamic" || !map.fog.remember {
        return Ok(StatusCode::NO_CONTENT);
    }
    let (columns, rows) = grid::cells(map.width, map.height, &map.grid);
    let viewer = explorer(&db, &access, &id)?;
    let mut seen = unpack(&explored_cells(&db, &scene, &viewer)?);
    seen.extend(
        unpack(&body.cells)
            .into_iter()
            .filter(|&(x, y)| x < columns && y < rows),
    );
    db.execute(
        "INSERT INTO explored (scene_id, table_id, viewer, cells, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (scene_id, viewer) DO UPDATE SET cells = ?4, updated_at = ?5",
        params![scene, id, viewer, pack(&seen), ids::now()],
    )?;
    Ok(StatusCode::NO_CONTENT)
}

/// The DM forgets what was explored, for one viewer or everyone.
pub async fn forget_explored(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Query(query): Query<ViewerQuery>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    {
        let db = state.db();
        reach(&db, &access, &id, &scene)?;
        match &query.viewer {
            Some(viewer) => db.execute(
                "DELETE FROM explored WHERE scene_id = ?1 AND viewer = ?2",
                [&scene, viewer],
            )?,
            None => db.execute("DELETE FROM explored WHERE scene_id = ?1", [&scene])?,
        };
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_explored_cells_like_the_website() {
        let cells = unpack("0:0-2;1:0,4-5;x:1;2:a-b;3:7-6");
        assert_eq!(cells.len(), 6);
        assert_eq!(pack(&cells), "0:0-2;1:0,4-5");
        assert!(unpack("").is_empty());
    }
}
