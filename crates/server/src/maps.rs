//! The table's maps (scenes). The DM keeps several and picks the one the players see; players
//! act only on that one, and only as far as the DM lets them. Every change rewrites the map
//! while holding the database, so two moves at once can't undo each other.

use crate::access::{self, Access, Credentials};
use crate::error::{AppError, Result};
use crate::ids;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use yuki_vtt_rules::map::{self, MapGrid};

const MOST_SCENES: i64 = 30;
const MOST_TOKENS: usize = 200;
const MOST_PROPS: usize = 200;
const MOST_DRAWINGS: usize = 300;
const MOST_POINTS: usize = 2000;
const TOKEN_COLORS: [&str; 6] = [
    "#86d5ed", "#ff9090", "#9be39b", "#f2d27a", "#c9a0f0", "#f0f0f0",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Image {
    pub mime: String,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Token {
    pub id: String,
    pub name: String,
    /// Its top left corner, in cells: whole cells on the grid, anywhere between when placed
    /// freely (to a hundredth of a cell).
    #[serde(serialize_with = "whole")]
    pub x: f64,
    #[serde(serialize_with = "whole")]
    pub y: f64,
    /// Cells across, by halves: 0.5 for Tiny, 1 Small or Medium, 2 Large, 3 Huge, 4 Gargantuan.
    #[serde(serialize_with = "whole")]
    pub size: f64,
    pub color: String,
    /// A seated character's token; its player may move it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
    /// Hidden tokens are the DM's alone: players never receive them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    /// What it sees when the map has fog, in feet. Without one, a seated character sees by
    /// their darkvision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<Vision>,
    /// A light it carries, like a torch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<Carried>,
    /// Seats that see through it too (a familiar), beside its own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_with: Option<Vec<String>>,
    /// Which way it faces, in degrees, for cones of light and sight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facing: Option<f64>,
    /// The token it rides on or is carried by: it moves when that one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached_to: Option<String>,
    /// What it follows from that holder: position by default, plus any of these it chooses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<Vec<String>>,
    /// Its name not written under it on the map (players never get it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hidden: Option<bool>,
    /// Coloured dots on it, for whatever the table agrees they mean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marks: Option<Vec<String>>,
    /// Its picture turned (0, 90, 180 or 270 degrees) or mirrored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flip: Option<bool>,
    /// A number shown on it, for whatever the table counts: arrows left, rounds, charges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter: Option<i32>,
}

// A picture turned to any whole degree; 0 (and 360) leaves it unturned.
fn turn(value: u32) -> Result<Option<u32>> {
    match value {
        0 | 360 => Ok(None),
        1..=359 => Ok(Some(value)),
        _ => Err(AppError::bad("A picture turns 0 to 360 degrees")),
    }
}

const MARKS: [&str; 6] = ["red", "orange", "yellow", "green", "blue", "purple"];

const FOLLOW: [&str; 5] = ["position", "rotation", "size", "visibility", "lock"];

/// What an attachment follows from its holder: position only unless it chooses more.
fn follows(rider: &Token, what: &str) -> bool {
    match &rider.follow {
        None => what == "position",
        Some(list) => list.iter().any(|f| f == what),
    }
}

/// The tokens carried along when one moves: those attached to it, and those attached to them.
pub fn carried_by(tokens: &[Token], id: &str) -> std::collections::HashSet<String> {
    let mut carried = std::collections::HashSet::new();
    let mut holding = vec![id.to_string()];
    while let Some(holder) = holding.pop() {
        for t in tokens {
            if t.attached_to.as_deref() == Some(holder.as_str())
                && t.id != id
                && carried.insert(t.id.clone())
            {
                holding.push(t.id.clone());
            }
        }
    }
    carried
}

/// A list with one thing moved to the end (drawn on top) or the start (drawn under).
fn restack<T: Clone>(items: &mut Vec<T>, at: usize, layer: &str) {
    let item = items.remove(at);
    if layer == "front" {
        items.push(item);
    } else {
        items.insert(0, item);
    }
}

// A layer as sent: front or back.
fn layer(value: Option<&str>) -> Result<Option<&str>> {
    match value {
        None | Some("front" | "back") => Ok(value),
        Some(_) => Err(AppError::bad("A layer is front or back")),
    }
}

/// How far a token sees in the dark: clear out to the range, fading over the falloff, in feet.
/// A cone looks the way the token faces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vision {
    pub range: u32,
    pub falloff: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
}

/// A light's reach in feet: bright out to one radius, dim to the other. A cone (a bullseye
/// lantern) shines the way its token faces; a sharp edge stops instead of fading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Carried {
    pub bright: u32,
    pub dim: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge: Option<String>,
}

/// Whether a light's or a sight's spread is one the map knows: all around or a cone of 5 to
/// 360 degrees, soft or sharp.
pub(crate) fn spread_ok(shape: &Option<String>, width: Option<f64>, edge: &Option<String>) -> bool {
    shape
        .as_deref()
        .is_none_or(|s| s == "radius" || s == "cone")
        && width.is_none_or(|w| w.is_finite() && (5.0..=360.0).contains(&w))
        && edge.as_deref().is_none_or(|e| e == "soft" || e == "sharp")
}

/// How a map's fog works: off, or dynamic on a bright or dark map, remembering explored
/// places or not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fog {
    pub mode: String,
    pub light: String,
    pub remember: bool,
    /// Walls, windows and closed doors stop players' tokens; on unless the DM turns it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collide: Option<bool>,
    /// Painted fog: the whole map starts hidden, and the DM reveals it area by area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<bool>,
    /// The fog's colour; none means the default dark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// How painted areas combine: stacked in order, or single-layer where a new shape cuts
    /// the ones under it. Stacked unless set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
}

fn no_fog() -> Fog {
    Fog {
        mode: "off".into(),
        light: "bright".into(),
        remember: true,
        collide: None,
        cover: None,
        color: None,
        layer: None,
    }
}

/// A wall that stops sight, in the picture's pixels. A door is a wall that can be open.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wall {
    pub id: String,
    pub a: [f64; 2],
    pub b: [f64; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub door: Option<Door>,
    /// A window: sight and light go through it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub window: bool,
}

/// A door: a locked one stays shut until the DM unlocks it; a secret one is a plain wall to
/// players while it is closed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Door {
    pub open: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
}

/// A light on the map, like a campfire, or a cone of light aimed one way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Light {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub bright: u32,
    pub dim: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge: Option<String>,
    /// Where a cone points, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f64>,
}

/// A note pinned on the map for the DM alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub text: String,
    /// Players can read it too; otherwise it is the DM's alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
}

/// A spell's area: a cone or line from where it starts, a sphere around a point, a cube with
/// that point on one face. Sizes in feet; the angle is where it points, in degrees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub id: String,
    pub shape: String,
    pub x: f64,
    pub y: f64,
    pub size: u32,
    #[serde(default = "five")]
    pub width: u32,
    #[serde(default)]
    pub angle: f64,
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Who placed it: a seat, or "dm". They may move or take it away.
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
}

fn five() -> u32 {
    5
}

/// A picture laid on the map that isn't a token: a door, a chest, a cart. Sized in cells.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prop {
    pub id: String,
    pub asset_id: String,
    pub x: u32,
    pub y: u32,
    /// Cells across and down: any size from half a cell to 40.
    #[serde(serialize_with = "whole")]
    pub w: f64,
    #[serde(serialize_with = "whole")]
    pub h: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flip: Option<bool>,
}

/// A line by hand, or a shape from its first point to its last, in the picture's pixels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drawing {
    pub id: String,
    pub shape: String,
    pub color: String,
    pub points: Vec<[f64; 2]>,
    /// Who drew it: a seat, or "dm" for whoever runs the table. They may erase it.
    pub by: String,
    /// On the DM's layer: players never receive it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// How heavy its line is: thin, normal or thick; normal when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<String>,
    /// Words on the map: a text drawing's, at its first point (its weight is its size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableMap {
    pub width: u32,
    pub height: u32,
    pub grid: MapGrid,
    pub image: Option<Image>,
    #[serde(default)]
    pub tokens: Vec<Token>,
    #[serde(default)]
    pub props: Vec<Prop>,
    #[serde(default)]
    pub drawings: Vec<Drawing>,
    #[serde(default = "no_fog")]
    pub fog: Fog,
    #[serde(default)]
    pub walls: Vec<Wall>,
    #[serde(default)]
    pub lights: Vec<Light>,
    /// The DM's notes; players never receive them.
    #[serde(default)]
    pub notes: Vec<Note>,
    #[serde(default)]
    pub templates: Vec<Template>,
    /// The DM's painted fog, area by area.
    #[serde(default, rename = "fogAreas")]
    pub fog_areas: Vec<FogArea>,
    /// Rulers pinned to the map for everyone.
    #[serde(default)]
    pub rulers: Vec<Ruler>,
}

/// A ruler someone pinned to the map, kept for everyone until they or the DM take it off: the
/// middles of the cells it runs through, corner by corner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ruler {
    pub id: String,
    pub points: Vec<[f64; 2]>,
    /// Who pinned it: a seat, or "dm".
    pub by: String,
}

/// An area of fog the DM painted: hidden from players, or (reveal) cut out of the fog. Later
/// areas go over earlier ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FogArea {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<bool>,
    pub points: Vec<[f64; 2]>,
}

/// Whether a point is inside a shape of any form (even-odd: count the edges a ray crosses).
pub fn inside_shape(points: &[[f64; 2]], (x, y): (f64, f64)) -> bool {
    let mut inside = false;
    let mut j = points.len().wrapping_sub(1);
    for i in 0..points.len() {
        let ([xi, yi], [xj, yj]) = (points[i], points[j]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

impl TableMap {
    /// Whether the DM's painted fog hides a spot from players.
    pub fn hand_fogged(&self, at: (f64, f64)) -> bool {
        let mut hidden = self.fog.cover == Some(true);
        for area in &self.fog_areas {
            if inside_shape(&area.points, at) {
                hidden = area.reveal != Some(true);
            }
        }
        hidden
    }
}

/// What the DM sets for a map: its size and grid.
#[derive(Debug, Clone, Deserialize)]
pub struct Setup {
    pub width: u32,
    pub height: u32,
    pub grid: MapGrid,
}

impl Setup {
    fn check(&self) -> Result<()> {
        let g = &self.grid;
        let ok = (100..=16_384).contains(&self.width)
            && (100..=16_384).contains(&self.height)
            && (8..=400).contains(&g.size)
            && g.offset_x <= 399
            && g.offset_y <= 399
            && (1..=100).contains(&g.feet_per_cell);
        if !ok {
            return Err(AppError::bad(
                "A map is 100 to 16384 pixels, with cells of 8 to 400",
            ));
        }
        if let Some(s) = &g.style {
            let colour = s.color.len() == 7
                && s.color.starts_with('#')
                && s.color[1..].chars().all(|c| c.is_ascii_hexdigit());
            if !colour || !(0.5..=6.0).contains(&s.width) || !(0.05..=1.0).contains(&s.opacity) {
                return Err(AppError::bad(
                    "Grid lines take a #rrggbb colour, width 0.5 to 6, opacity 0.05 to 1",
                ));
            }
        }
        Ok(())
    }
}

// A blank map to start from: 20 by 14 cells of 70 pixels, 5 feet each.
fn blank() -> TableMap {
    TableMap {
        width: 1400,
        height: 980,
        grid: MapGrid {
            size: 70,
            offset_x: 0,
            offset_y: 0,
            feet_per_cell: 5,
            show: true,
            diagonals: None,
            style: None,
        },
        image: None,
        tokens: vec![],
        props: vec![],
        drawings: vec![],
        fog: no_fog(),
        walls: vec![],
        lights: vec![],
        notes: vec![],
        templates: vec![],
        fog_areas: vec![],
        rulers: vec![],
    }
}

impl TableMap {
    fn cells(&self) -> (u32, u32) {
        map::cells(self.width, self.height, &self.grid)
    }

    // A token stays whole on the map, also after the map shrinks.
    // A token kept on the map, where it was put (to a hundredth of a cell).
    fn keep_token(&self, mut token: Token) -> Token {
        let (columns, rows) = self.cells();
        let keep = |at: f64, most: u32| {
            ((at * 100.0).round() / 100.0).clamp(0.0, (f64::from(most) - token.size).max(0.0))
        };
        token.x = keep(token.x, columns);
        token.y = keep(token.y, rows);
        token
    }

    fn keep_prop(&self, mut prop: Prop) -> Prop {
        let (columns, rows) = self.cells();
        let (cols, rows) = (f64::from(columns), f64::from(rows));
        prop.w = ((prop.w.min(cols)) * 100.0).round() / 100.0;
        prop.h = ((prop.h.min(rows)) * 100.0).round() / 100.0;
        prop.x = prop.x.min((cols - prop.w).floor().max(0.0) as u32);
        prop.y = prop.y.min((rows - prop.h).floor().max(0.0) as u32);
        prop
    }

    fn keep_all(mut self) -> Self {
        let tokens = std::mem::take(&mut self.tokens);
        self.tokens = tokens.into_iter().map(|t| self.keep_token(t)).collect();
        let props = std::mem::take(&mut self.props);
        self.props = props.into_iter().map(|p| self.keep_prop(p)).collect();
        self
    }
}

pub(crate) fn credentials(headers: &HeaderMap) -> Credentials {
    Credentials::from(headers, None)
}

pub(crate) fn not_allowed(what: &str) -> AppError {
    AppError::not_found(format!("The DM hasn't let you {what}"))
}

pub(crate) fn allow(allowed: bool, what: &str) -> Result<()> {
    if allowed {
        Ok(())
    } else {
        Err(not_allowed(what))
    }
}

pub(crate) fn color(value: &str) -> Result<String> {
    let ok = value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(value.to_lowercase())
    } else {
        Err(AppError::bad("A colour is like #86d5ed"))
    }
}

fn token_name(value: &str) -> Result<String> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(AppError::bad("A token's name is 1 to 60 letters"));
    }
    Ok(name.to_string())
}

fn cell(x: f64, y: f64) -> Result<()> {
    if !(0.0..=1000.0).contains(&x) || !(0.0..=1000.0).contains(&y) {
        Err(AppError::bad("That's off the map"))
    } else {
        Ok(())
    }
}

fn size(size: f64) -> Result<f64> {
    if (0.5..=4.0).contains(&size) {
        Ok((size * 100.0).round() / 100.0)
    } else {
        Err(AppError::bad("A token is half a cell to 4 cells across"))
    }
}

/// A number as JSON: whole ones without a ".0", as the website sends them.
fn whole<S: serde::Serializer>(value: &f64, out: S) -> std::result::Result<S::Ok, S::Error> {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        out.serialize_i64(*value as i64)
    } else {
        out.serialize_f64(*value)
    }
}

/// A scene's map as stored, or "gone".
pub fn load(db: &Connection, table_id: &str, scene_id: &str) -> Result<TableMap> {
    let text: String = db
        .query_row(
            "SELECT data FROM scenes WHERE table_id = ?1 AND id = ?2",
            [table_id, scene_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("That map is gone"))?;
    Ok(serde_json::from_str(&text)?)
}

fn store(db: &Connection, scene_id: &str, map: &TableMap) -> Result<()> {
    db.execute(
        "UPDATE scenes SET data = ?2 WHERE id = ?1",
        params![scene_id, serde_json::to_string(map)?],
    )?;
    Ok(())
}

/// The scene with its id and name, as the table view sends it.
pub fn scene(db: &Connection, table_id: &str, scene_id: &str) -> Result<Option<Value>> {
    let row: Option<(String, String, Option<String>)> = db
        .query_row(
            "SELECT name, data, folder FROM scenes WHERE table_id = ?1 AND id = ?2",
            [table_id, scene_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((name, data, folder)) = row else {
        return Ok(None);
    };
    // Read as a map, so one saved before newer parts (fog, walls) gets them as defaults.
    let mut scene = serde_json::to_value(serde_json::from_str::<TableMap>(&data)?)?;
    scene["id"] = json!(scene_id);
    scene["name"] = json!(name);
    if let Some(folder) = folder {
        scene["folder"] = json!(folder);
    }
    Ok(Some(scene))
}

/// Every scene, by name, in order; for whoever runs the table.
pub fn list(db: &Connection, table_id: &str) -> Result<Vec<Value>> {
    let mut rows = db.prepare(
        "SELECT id, name, folder FROM scenes WHERE table_id = ?1 ORDER BY position, created_at",
    )?;
    let scenes = rows
        .query_map([table_id], |row| {
            let mut scene =
                json!({ "id": row.get::<_, String>(0)?, "name": row.get::<_, String>(1)? });
            if let Some(folder) = row.get::<_, Option<String>>(2)? {
                scene["folder"] = json!(folder);
            }
            Ok(scene)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(scenes)
}

pub(crate) fn active_scene(db: &Connection, table_id: &str) -> Result<Option<String>> {
    Ok(db.query_row(
        "SELECT active_scene_id FROM tables WHERE id = ?1",
        [table_id],
        |row| row.get(0),
    )?)
}

// Changes a map. Players act on the map everyone sees, never on the DM's others.
pub(crate) fn edit(
    state: &AppState,
    access: &Access,
    table_id: &str,
    scene_id: &str,
    change: impl FnOnce(TableMap) -> Result<TableMap>,
) -> Result<StatusCode> {
    {
        let db = state.db();
        if !access.runs() && active_scene(&db, table_id)?.as_deref() != Some(scene_id) {
            return Err(AppError::not_found("That map is gone"));
        }
        let changed = change(load(&db, table_id, scene_id)?)?;
        store(&db, scene_id, &changed)?;
    }
    state.events.changed(table_id);
    Ok(StatusCode::NO_CONTENT)
}

fn picture_exists(db: &Connection, table_id: &str, asset_id: &str) -> Result<()> {
    let found: i64 = db.query_row(
        "SELECT COUNT(*) FROM assets WHERE table_id = ?1 AND id = ?2",
        [table_id, asset_id],
        |row| row.get(0),
    )?;
    if found == 0 {
        Err(AppError::not_found("That picture is gone"))
    } else {
        Ok(())
    }
}

#[derive(Deserialize)]
pub struct NewScene {
    name: String,
    setup: Option<Setup>,
    folder: Option<String>,
}

fn scene_name(value: &str) -> Result<String> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(AppError::bad("A map's name is 1 to 60 letters"));
    }
    Ok(name.to_string())
}

fn scene_folder(value: &str) -> Result<String> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > 40 {
        return Err(AppError::bad("A folder's name is 1 to 40 letters"));
    }
    Ok(name.to_string())
}

/// `POST /api/tables/{id}/scenes`: a new map; the first is the one everyone sees.
pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<NewScene>,
) -> Result<Json<Value>> {
    access::runner(&state, &id, &credentials(&headers))?;
    let name = scene_name(&body.name)?;
    let folder = body.folder.as_deref().map(scene_folder).transpose()?;
    let mut new_map = blank();
    if let Some(setup) = body.setup {
        setup.check()?;
        (new_map.width, new_map.height, new_map.grid) = (setup.width, setup.height, setup.grid);
    }
    let scene_id = ids::id();
    {
        let db = state.db();
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM scenes WHERE table_id = ?1",
            [&id],
            |row| row.get(0),
        )?;
        if count >= MOST_SCENES {
            return Err(AppError::bad(format!("A table keeps {MOST_SCENES} maps")));
        }
        db.execute(
            "INSERT INTO scenes (id, table_id, name, folder, position, data, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![scene_id, id, name, folder, count, serde_json::to_string(&new_map)?, ids::now()],
        )?;
        db.execute(
            "UPDATE tables SET active_scene_id = ?2 WHERE id = ?1 AND active_scene_id IS NULL",
            [&id, &scene_id],
        )?;
    }
    state.events.changed(&id);
    let mut made = json!({ "id": scene_id, "name": name });
    if let Some(folder) = folder {
        made["folder"] = json!(folder);
    }
    Ok(Json(made))
}

#[derive(Deserialize)]
pub struct SceneChange {
    name: Option<String>,
    setup: Option<Setup>,
    /// A folder's name, or null to move the map out of its folder.
    #[serde(default, deserialize_with = "present")]
    folder: Option<Value>,
}

/// Renames a map, moves it to a folder, or changes its size and grid; tokens stay whole on it.
pub async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<SceneChange>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    if let Some(name) = &body.name {
        let name = scene_name(name)?;
        let changed = state.db().execute(
            "UPDATE scenes SET name = ?3 WHERE table_id = ?1 AND id = ?2",
            [&id, &scene, &name],
        )?;
        if changed == 0 {
            return Err(AppError::not_found("That map is gone"));
        }
    }
    if let Some(folder) = &body.folder {
        let folder = match folder {
            Value::Null => None,
            Value::String(name) => Some(scene_folder(name)?),
            _ => return Err(AppError::bad("A folder is a name, or null")),
        };
        let changed = state.db().execute(
            "UPDATE scenes SET folder = ?3 WHERE table_id = ?1 AND id = ?2",
            params![id, scene, folder],
        )?;
        if changed == 0 {
            return Err(AppError::not_found("That map is gone"));
        }
    }
    let Some(setup) = body.setup else {
        state.events.changed(&id);
        return Ok(StatusCode::NO_CONTENT);
    };
    setup.check()?;
    edit(&state, &access, &id, &scene, |map| {
        Ok(TableMap {
            width: setup.width,
            height: setup.height,
            grid: setup.grid,
            ..map
        }
        .keep_all())
    })
}

/// Deletes a map; if the players were looking at it, they see the next one.
pub async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    {
        let db = state.db();
        if db.execute(
            "DELETE FROM scenes WHERE table_id = ?1 AND id = ?2",
            [&id, &scene],
        )? == 0
        {
            return Err(AppError::not_found("That map is gone"));
        }
        if active_scene(&db, &id)?.as_deref() == Some(scene.as_str()) {
            let next = list(&db, &id)?
                .first()
                .and_then(|s| s["id"].as_str().map(String::from));
            db.execute(
                "UPDATE tables SET active_scene_id = ?2 WHERE id = ?1",
                params![id, next],
            )?;
        }
    }
    crate::pictures::forget_map(&state, &id, &scene);
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/tables/{id}/scenes/{scene}/show`: the map the players see from now on.
pub async fn show(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &credentials(&headers))?;
    {
        let db = state.db();
        load(&db, &id, &scene)?;
        db.execute(
            "UPDATE tables SET active_scene_id = ?2 WHERE id = ?1",
            [&id, &scene],
        )?;
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// The map got a new picture: its size is the picture's, and tokens stay on it.
pub fn set_picture(
    db: &Connection,
    table_id: &str,
    scene_id: &str,
    mime: &str,
    width: u32,
    height: u32,
) -> Result<()> {
    let current = load(db, table_id, scene_id)?;
    let image = Some(Image {
        mime: mime.into(),
        version: ids::now(),
    });
    store(
        db,
        scene_id,
        &TableMap {
            width,
            height,
            image,
            ..current
        }
        .keep_all(),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewToken {
    name: String,
    x: f64,
    y: f64,
    #[serde(default = "one")]
    size: f64,
    color: Option<String>,
    image_id: Option<String>,
    /// On the DM's layer from the start, as a copy of a hidden monster is.
    #[serde(default)]
    hidden: bool,
}

fn one() -> f64 {
    1.0
}

pub async fn add_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewToken>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("tokens"), "add tokens")?;
    if body.hidden {
        allow(access.runs(), "put tokens on the DM's layer")?;
    }
    cell(body.x, body.y)?;
    let token = Token {
        id: format!("tok-{}", ids::id()),
        name: token_name(&body.name)?,
        x: body.x,
        y: body.y,
        size: size(body.size)?,
        color: color(body.color.as_deref().unwrap_or(TOKEN_COLORS[1]))?,
        seat_id: None,
        image_id: body.image_id,
        hidden: body.hidden.then_some(true),
        locked: None,
        vision: None,
        light: None,
        shared_with: None,
        facing: None,
        attached_to: None,
        follow: None,
        name_hidden: None,
        marks: None,
        rotation: None,
        flip: None,
        counter: None,
    };
    if let Some(image) = &token.image_id {
        picture_exists(&state.db(), &id, image)?;
    }
    edit(&state, &access, &id, &scene, |mut map| {
        if map.tokens.len() >= MOST_TOKENS {
            return Err(AppError::bad(format!("A map holds {MOST_TOKENS} tokens")));
        }
        let token = map.keep_token(token);
        map.tokens.push(token);
        Ok(map)
    })
}

/// Every seated character not on this map yet gets a token, along the top row.
pub async fn place_party(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    let seated: Vec<(String, String)> = {
        let db = state.db();
        let mut rows =
            db.prepare("SELECT id, snapshot FROM seats WHERE table_id = ?1 ORDER BY created_at")?;
        rows.query_map([&id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|(seat, text)| {
            let snapshot: Value = serde_json::from_str(&text)?;
            Ok((
                seat,
                snapshot["summary"]["name"]
                    .as_str()
                    .unwrap_or("Guest")
                    .to_string(),
            ))
        })
        .collect::<Result<_>>()?
    };
    edit(&state, &access, &id, &scene, |mut map| {
        let placed: Vec<String> = map
            .tokens
            .iter()
            .filter_map(|t| t.seat_id.clone())
            .collect();
        let newcomers = seated
            .into_iter()
            .filter(|(seat, _)| !placed.contains(seat));
        for (i, (seat, name)) in newcomers.enumerate() {
            let name: String = name.chars().take(60).collect();
            let token = Token {
                id: format!("tok-{}", ids::id()),
                name,
                x: i as f64,
                y: 0.0,
                size: 1.0,
                color: TOKEN_COLORS[0].into(),
                seat_id: Some(seat),
                image_id: None,
                hidden: None,
                locked: None,
                vision: None,
                light: None,
                shared_with: None,
                facing: None,
                attached_to: None,
                follow: None,
                name_hidden: None,
                marks: None,
                rotation: None,
                flip: None,
                counter: None,
            };
            let token = map.keep_token(token);
            map.tokens.push(token);
        }
        map.tokens.truncate(MOST_TOKENS);
        Ok(map)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenChange {
    x: Option<f64>,
    y: Option<f64>,
    name: Option<String>,
    size: Option<f64>,
    color: Option<String>,
    /// A picture's id, or null to take the picture off.
    #[serde(default, deserialize_with = "present")]
    image_id: Option<Value>,
    hidden: Option<bool>,
    locked: Option<bool>,
    /// What it sees, or null for its character's darkvision.
    #[serde(default, deserialize_with = "present")]
    vision: Option<Value>,
    /// A light it carries, or null for none.
    #[serde(default, deserialize_with = "present")]
    light: Option<Value>,
    shared_with: Option<Vec<String>>,
    facing: Option<f64>,
    /// The token it is attached to, or null to let go.
    #[serde(default, deserialize_with = "present")]
    attached_to: Option<Value>,
    follow: Option<Vec<String>>,
    layer: Option<String>,
    name_hidden: Option<bool>,
    marks: Option<Vec<String>>,
    rotation: Option<u32>,
    flip: Option<bool>,
    /// A new number on it, or null to take it off.
    #[serde(default, deserialize_with = "present")]
    counter: Option<Value>,
}

// A field that was sent, null included: Option alone reads null as not sent, so "take the
// picture off" would change nothing. A field left out stays None.
fn present<'de, D: serde::Deserializer<'de>>(
    sent: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(sent).map(Some)
}

// Distances a person sets on the map, in feet.
fn feet(value: u32) -> Result<u32> {
    if value <= 1000 {
        Ok(value)
    } else {
        Err(AppError::bad("Distances on the map are 0 to 1000 feet"))
    }
}

// A new sight or light for a token: the shape, or null to take it away.
fn sight_part<T: serde::de::DeserializeOwned>(
    value: &Option<Value>,
    what: &str,
) -> Result<Option<Option<T>>> {
    match value {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(value) => serde_json::from_value(value.clone())
            .map(|part| Some(Some(part)))
            .map_err(|_| AppError::bad(format!("That isn't a token's {what}"))),
    }
}

/// Moving takes "move-own" for your own character's token, "move-any" for the rest; changing a
/// token's name, size, colour or picture takes "tokens"; hiding and locking are the DM's.
pub async fn change_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, token_id)): Path<(String, String, String)>,
    Json(body): Json<TokenChange>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    let vision = sight_part::<Vision>(&body.vision, "sight")?;
    let light = sight_part::<Carried>(&body.light, "light")?;
    if let Some(Some(v)) = &vision {
        feet(v.range)?;
        feet(v.falloff)?;
        if !spread_ok(&v.shape, v.width, &None) {
            return Err(AppError::bad("A cone of sight is 5 to 360 degrees"));
        }
    }
    if let Some(Some(l)) = &light {
        feet(l.bright)?;
        feet(l.dim)?;
        if !spread_ok(&l.shape, l.width, &l.edge) {
            return Err(AppError::bad(
                "A cone of light is 5 to 360 degrees, soft or sharp",
            ));
        }
    }
    if body
        .facing
        .is_some_and(|f| !f.is_finite() || !(-360.0..=360.0).contains(&f))
    {
        return Err(AppError::bad("A token faces -360 to 360 degrees"));
    }
    if let Some(seats) = &body.shared_with
        && (seats.len() > 20 || seats.iter().any(|s| s.len() > 40))
    {
        return Err(AppError::bad("A token is shared with up to 20 seats"));
    }
    let counter = match &body.counter {
        None => None,
        Some(Value::Null) => Some(None),
        Some(value) => match value.as_i64() {
            Some(n) if (-99..=999).contains(&n) => Some(Some(n as i32)),
            _ => return Err(AppError::bad("A token's number is a whole -99 to 999")),
        },
    };
    let picture = match &body.image_id {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(image)) => {
            picture_exists(&state.db(), &id, image)?;
            Some(Some(image.clone()))
        }
        Some(_) => return Err(AppError::bad("A token's picture is a picture's id")),
    };
    edit(&state, &access, &id, &scene, |mut map| {
        let at = map
            .tokens
            .iter()
            .position(|t| t.id == token_id && (access.runs() || t.hidden != Some(true)))
            .ok_or_else(|| AppError::not_found("That token is gone"))?;
        let mut token = map.tokens[at].clone();
        // Turning a token is moving it, as far as who may.
        let moving = body.x.is_some() || body.y.is_some() || body.facing.is_some();
        let restyling = body.name.is_some()
            || body.size.is_some()
            || body.color.is_some()
            || picture.is_some()
            || body.name_hidden.is_some()
            || body.marks.is_some()
            || body.rotation.is_some()
            || body.flip.is_some()
            || counter.is_some();
        let mine = token
            .seat_id
            .as_ref()
            .is_some_and(|seat| access.seat_ids.contains(seat));
        if moving {
            allow(
                access.may("move-any") || (mine && access.may("move-own")),
                "move it",
            )?;
            if token.locked == Some(true) && body.locked != Some(false) {
                return Err(AppError::bad(format!("{} is locked", token.name)));
            }
            token.x = body.x.unwrap_or(token.x);
            token.y = body.y.unwrap_or(token.y);
            cell(token.x, token.y)?;
        }
        if restyling {
            allow(access.may("tokens"), "change tokens")?;
        }
        if body.hidden.is_some() || body.locked.is_some() {
            allow(access.runs(), "hide or lock tokens")?;
        }
        if vision.is_some() || light.is_some() || body.shared_with.is_some() {
            allow(access.runs(), "change a token's sight")?;
        }
        if let Some(vision) = vision {
            token.vision = vision;
        }
        if let Some(light) = light {
            token.light = light;
        }
        if let Some(seats) = &body.shared_with {
            token.shared_with = (!seats.is_empty()).then(|| seats.clone());
        }
        if let Some(facing) = body.facing {
            token.facing = Some(facing.round());
        }
        if let Some(name) = &body.name {
            token.name = token_name(name)?;
        }
        if let Some(new_size) = body.size {
            token.size = size(new_size)?;
        }
        if let Some(new_color) = &body.color {
            token.color = color(new_color)?;
        }
        if let Some(hidden) = body.name_hidden {
            token.name_hidden = hidden.then_some(true);
        }
        if let Some(rotation) = body.rotation {
            token.rotation = turn(rotation)?;
        }
        if let Some(flip) = body.flip {
            token.flip = flip.then_some(true);
        }
        if let Some(number) = counter {
            token.counter = number;
        }
        if let Some(marks) = &body.marks {
            let known = marks.iter().all(|m| MARKS.contains(&m.as_str()));
            if !known || marks.len() > MARKS.len() {
                return Err(AppError::bad(
                    "Markers are red, orange, yellow, green, blue or purple",
                ));
            }
            token.marks = (!marks.is_empty()).then(|| marks.clone());
        }
        if let Some(image) = picture {
            token.image_id = image;
        }
        if let Some(hidden) = body.hidden {
            token.hidden = hidden.then_some(true);
        }
        if let Some(locked) = body.locked {
            token.locked = locked.then_some(true);
        }
        // Attaching it to another: one that is there, and never round in a loop.
        if body.attached_to.is_some() || body.follow.is_some() || body.layer.is_some() {
            allow(access.may("tokens"), "arrange tokens")?;
        }
        match &body.attached_to {
            Some(Value::Null) => token.attached_to = None,
            Some(Value::String(holder)) => {
                let there = map.tokens.iter().any(|t| &t.id == holder);
                if !there
                    || holder == &token_id
                    || carried_by(&map.tokens, &token_id).contains(holder)
                {
                    return Err(AppError::bad("It can't be attached to that"));
                }
                token.attached_to = Some(holder.clone());
            }
            Some(_) => return Err(AppError::bad("Attached to a token's id, or null")),
            None => {}
        }
        if let Some(follow) = &body.follow {
            let known = follow.iter().all(|f| FOLLOW.contains(&f.as_str()));
            if !known || follow.len() > FOLLOW.len() {
                return Err(AppError::bad(
                    "It follows position, rotation, size, visibility or lock",
                ));
            }
            token.follow = Some(follow.clone());
        }
        let stack = layer(body.layer.as_deref())?.map(str::to_string);
        let placed = map.keep_token(token);
        let before = map.tokens[at].clone();
        // Walls stop players' tokens (whoever runs the table goes anywhere).
        let walled = !access.runs() && map.fog.collide != Some(false) && !map.walls.is_empty();
        if walled && (placed.x, placed.y) != (before.x, before.y) {
            let from = crate::fog::middle_of(&map, &before);
            if wall_in_the_way(&map.walls, from, crate::fog::middle_of(&map, &placed)) {
                return Err(AppError::bad("A wall is in the way"));
            }
        }
        // What it carries takes from it what each rider chooses to follow.
        let (dx, dy) = (placed.x - before.x, placed.y - before.y);
        let carried = carried_by(&map.tokens, &token_id);
        let after = placed.clone();
        map.tokens[at] = placed;
        for i in 0..map.tokens.len() {
            if carried.contains(&map.tokens[i].id) {
                let mut rider = map.tokens[i].clone();
                if follows(&rider, "position") {
                    rider.x = (rider.x + dx).max(0.0);
                    rider.y = (rider.y + dy).max(0.0);
                }
                if follows(&rider, "rotation") {
                    if after.facing != before.facing {
                        rider.facing = after.facing;
                    }
                    if after.rotation != before.rotation {
                        rider.rotation = after.rotation;
                    }
                }
                if follows(&rider, "size") && after.size != before.size {
                    rider.size = after.size;
                }
                if follows(&rider, "visibility") && after.hidden != before.hidden {
                    rider.hidden = after.hidden;
                }
                if follows(&rider, "lock") && after.locked != before.locked {
                    rider.locked = after.locked;
                }
                map.tokens[i] = map.keep_token(rider);
            }
        }
        if let Some(stack) = stack {
            restack(&mut map.tokens, at, &stack);
        }
        Ok(map)
    })
}

/// Whether a wall is in the way of a token going straight from one spot to another: walls,
/// windows and closed doors stop it, open doors don't. A wall it only touches at the start
/// (one it stands against) doesn't count.
pub fn wall_in_the_way(walls: &[Wall], from: (f64, f64), to: (f64, f64)) -> bool {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    if dx.hypot(dy) < 1.0 {
        return false;
    }
    walls.iter().any(|wall| {
        if wall.door.as_ref().is_some_and(|d| d.open) {
            return false;
        }
        let (ex, ey) = (wall.b[0] - wall.a[0], wall.b[1] - wall.a[1]);
        let across = dx * ey - dy * ex;
        if across.abs() < 1e-12 {
            return false;
        }
        let (px, py) = (wall.a[0] - from.0, wall.a[1] - from.1);
        let t = (px * ey - py * ex) / across;
        let u = (px * dy - py * dx) / across;
        t > 1e-6 && t <= 1.0 && (0.0..=1.0).contains(&u)
    })
}

pub async fn remove_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, token_id)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("tokens"), "remove tokens")?;
    edit(&state, &access, &id, &scene, |mut map| {
        // What it carried goes with it.
        let mut gone = carried_by(&map.tokens, &token_id);
        gone.insert(token_id.clone());
        map.tokens
            .retain(|t| !gone.contains(&t.id) || (!access.runs() && t.hidden == Some(true)));
        Ok(map)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProp {
    asset_id: String,
    x: u32,
    y: u32,
    w: f64,
    h: f64,
    locked: Option<bool>,
}

fn prop_size(w: f64, h: f64) -> Result<()> {
    if (0.5..=40.0).contains(&w) && (0.5..=40.0).contains(&h) {
        Ok(())
    } else {
        Err(AppError::bad(
            "A picture on the map is half a cell to 40 cells",
        ))
    }
}

pub async fn add_prop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewProp>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("tokens"), "place pictures")?;
    cell(f64::from(body.x), f64::from(body.y))?;
    prop_size(body.w, body.h)?;
    picture_exists(&state.db(), &id, &body.asset_id)?;
    let locked = body.locked.filter(|l| *l && access.runs());
    edit(&state, &access, &id, &scene, |mut map| {
        if map.props.len() >= MOST_PROPS {
            return Err(AppError::bad(format!("A map holds {MOST_PROPS} pictures")));
        }
        let prop = map.keep_prop(Prop {
            id: format!("prop-{}", ids::id()),
            asset_id: body.asset_id,
            x: body.x,
            y: body.y,
            w: body.w,
            h: body.h,
            locked,
            rotation: None,
            flip: None,
        });
        map.props.push(prop);
        Ok(map)
    })
}

#[derive(Deserialize)]
pub struct PropChange {
    x: Option<u32>,
    y: Option<u32>,
    w: Option<f64>,
    h: Option<f64>,
    locked: Option<bool>,
    layer: Option<String>,
    rotation: Option<u32>,
    flip: Option<bool>,
}

pub async fn change_prop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, prop_id)): Path<(String, String, String)>,
    Json(body): Json<PropChange>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("tokens"), "move pictures")?;
    if body.locked.is_some() {
        allow(access.runs(), "lock pictures")?;
    }
    edit(&state, &access, &id, &scene, |mut map| {
        let at = map
            .props
            .iter()
            .position(|p| p.id == prop_id)
            .ok_or_else(|| AppError::not_found("That picture is gone"))?;
        let mut prop = map.props[at].clone();
        let moving = body.x.is_some() || body.y.is_some() || body.w.is_some() || body.h.is_some();
        if moving && prop.locked == Some(true) && body.locked != Some(false) {
            return Err(AppError::bad("That picture is locked"));
        }
        (prop.x, prop.y) = (body.x.unwrap_or(prop.x), body.y.unwrap_or(prop.y));
        (prop.w, prop.h) = (body.w.unwrap_or(prop.w), body.h.unwrap_or(prop.h));
        cell(f64::from(prop.x), f64::from(prop.y))?;
        prop_size(prop.w, prop.h)?;
        if let Some(rotation) = body.rotation {
            prop.rotation = turn(rotation)?;
        }
        if let Some(flip) = body.flip {
            prop.flip = flip.then_some(true);
        }
        if let Some(locked) = body.locked {
            prop.locked = locked.then_some(true);
        }
        map.props[at] = map.keep_prop(prop);
        if let Some(stack) = layer(body.layer.as_deref())? {
            restack(&mut map.props, at, stack);
        }
        Ok(map)
    })
}

pub async fn remove_prop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, prop_id)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("tokens"), "remove pictures")?;
    edit(&state, &access, &id, &scene, |mut map| {
        map.props.retain(|p| p.id != prop_id);
        Ok(map)
    })
}

#[derive(Deserialize)]
pub struct NewDrawing {
    #[serde(default = "pen")]
    shape: String,
    color: String,
    points: Vec<[f64; 2]>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    weight: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

fn pen() -> String {
    "pen".into()
}

// A line's weight, as the website has them.
fn weight(value: Option<String>) -> Result<Option<String>> {
    match value.as_deref() {
        None => Ok(None),
        Some("thin" | "normal" | "thick") => Ok(value),
        Some(_) => Err(AppError::bad("A line is thin, normal or thick")),
    }
}

fn points_ok(points: &[[f64; 2]]) -> Result<()> {
    if !(2..=MOST_POINTS).contains(&points.len()) || points.iter().flatten().any(|n| !n.is_finite())
    {
        return Err(AppError::bad(format!(
            "A drawing has 2 to {MOST_POINTS} points"
        )));
    }
    Ok(())
}

pub async fn add_drawing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewDrawing>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("draw"), "draw")?;
    if body.hidden {
        allow(access.runs(), "draw on the DM's layer")?;
    }
    if !["pen", "line", "rect", "circle", "text", "fill"].contains(&body.shape.as_str()) {
        return Err(AppError::bad(
            "A drawing is a pen line, a line, a square, a circle, text or a filled shape",
        ));
    }
    points_ok(&body.points)?;
    let by = if access.runs() {
        "dm".to_string()
    } else {
        access
            .seat_ids
            .first()
            .cloned()
            .unwrap_or_else(|| "guest".into())
    };
    let text = words(body.text.as_deref(), &body.shape)?;
    let drawing = Drawing {
        id: format!("draw-{}", ids::id()),
        shape: body.shape,
        color: color(&body.color)?,
        points: body.points,
        by,
        hidden: body.hidden.then_some(true),
        weight: weight(body.weight)?,
        text,
    };
    edit(&state, &access, &id, &scene, |mut map| {
        map.drawings.push(drawing);
        // The oldest lines give way once a map is full of them.
        let extra = map.drawings.len().saturating_sub(MOST_DRAWINGS);
        map.drawings.drain(..extra);
        Ok(map)
    })
}

/// Whoever runs the table erases any line; a player only their own.
pub async fn remove_drawing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, drawing)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    edit(&state, &access, &id, &scene, |mut map| {
        if let Some(found) = map.drawings.iter().find(|d| d.id == drawing) {
            allow(
                access.runs() || access.seat_ids.contains(&found.by),
                "erase it",
            )?;
        }
        map.drawings.retain(|d| d.id != drawing);
        Ok(map)
    })
}

/// A drawing moved, reshaped, recoloured or hidden.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawingChange {
    points: Option<Vec<[f64; 2]>>,
    color: Option<String>,
    weight: Option<String>,
    hidden: Option<bool>,
    text: Option<String>,
}

// A text drawing's words: 1 to 200 letters; other drawings have none.
fn words(text: Option<&str>, shape: &str) -> Result<Option<String>> {
    match (text.map(str::trim), shape == "text") {
        (None, false) => Ok(None),
        (Some(t), true) if !t.is_empty() && t.chars().count() <= 200 => Ok(Some(t.to_string())),
        _ => Err(AppError::bad(
            "Text has 1 to 200 letters; other drawings have none",
        )),
    }
}

/// Whoever runs the table changes any drawing; a player only their own, never its layer.
pub async fn change_drawing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, drawing)): Path<(String, String, String)>,
    Json(body): Json<DrawingChange>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    if body.hidden.is_some() {
        allow(access.runs(), "move drawings to or from the DM's layer")?;
    }
    let color = body.color.as_deref().map(color).transpose()?;
    let weight = weight(body.weight)?;
    if let Some(points) = &body.points {
        points_ok(points)?;
    }
    edit(&state, &access, &id, &scene, |mut map| {
        let found = map
            .drawings
            .iter_mut()
            .find(|d| d.id == drawing)
            .ok_or_else(|| AppError::not_found("That drawing is gone"))?;
        allow(
            access.runs() || access.seat_ids.contains(&found.by),
            "change it",
        )?;
        if let Some(points) = body.points {
            // A shape stays its two points; a line by hand keeps however many it has.
            let two = ["line", "rect", "circle", "text"].contains(&found.shape.as_str());
            if two && points.len() != 2 {
                return Err(AppError::bad("A shape has two points"));
            }
            found.points = points;
        }
        if let Some(color) = color {
            found.color = color;
        }
        if weight.is_some() {
            found.weight = weight;
        }
        if let Some(hidden) = body.hidden {
            found.hidden = hidden.then_some(true);
        }
        if body.text.is_some() {
            found.text = words(body.text.as_deref(), &found.shape)?;
        }
        Ok(map)
    })
}

pub async fn clear_drawings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    edit(&state, &access, &id, &scene, |mut map| {
        map.drawings.clear();
        Ok(map)
    })
}

#[derive(Deserialize)]
pub struct Ping {
    x: f64,
    y: f64,
    /// Whoever runs the table moves everyone's view to the spot too.
    #[serde(default)]
    bring: bool,
}

/// Points at a spot on the map the players see; every screen shows it for a moment.
pub async fn ping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(at): Json<Ping>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("draw"), "ping")?;
    if at.bring {
        allow(access.runs(), "move everyone's view")?;
    }
    if !at.x.is_finite() || !at.y.is_finite() {
        return Err(AppError::bad("That's off the map"));
    }
    let scene =
        active_scene(&state.db(), &id)?.ok_or_else(|| AppError::bad("There is no map to ping"))?;
    let color = if access.runs() {
        TOKEN_COLORS[3]
    } else {
        TOKEN_COLORS[0]
    };
    let mut ping =
        json!({ "sceneId": scene, "x": at.x, "y": at.y, "by": access.name, "color": color });
    if at.bring {
        ping["bring"] = json!(true);
    }
    state.events.ping(&id, ping);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Mark {
    kind: String,
    points: Vec<[f64; 2]>,
    tab: String,
}

/// A ruler or pointer trail on the map shown: anyone measures, those who may draw point.
pub async fn mark(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Mark>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    match body.kind.as_str() {
        "ruler" => {}
        "trail" => allow(access.may("draw"), "point")?,
        _ => return Err(AppError::bad("A mark is a ruler or a trail")),
    }
    let fine = body.points.len() <= 50
        && body.points.iter().flatten().all(|n| n.is_finite())
        && !body.tab.is_empty()
        && body.tab.len() <= 40;
    if !fine {
        return Err(AppError::bad("A mark has up to 50 points"));
    }
    let scene =
        active_scene(&state.db(), &id)?.ok_or_else(|| AppError::bad("There is no map to mark"))?;
    let color = if access.runs() {
        TOKEN_COLORS[3]
    } else {
        TOKEN_COLORS[0]
    };
    let points: Vec<[f64; 2]> = body
        .points
        .iter()
        .map(|[x, y]| [x.round(), y.round()])
        .collect();
    state.events.mark(
        &id,
        json!({ "sceneId": scene, "kind": body.kind, "points": points, "by": access.name, "color": color, "tab": body.tab }),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Takes a deleted picture off every map.
pub fn forget_picture(db: &Connection, table_id: &str, asset_id: &str) -> Result<()> {
    let mut rows = db.prepare("SELECT id FROM scenes WHERE table_id = ?1")?;
    let scenes = rows
        .query_map([table_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for scene_id in scenes {
        let mut map = load(db, table_id, &scene_id)?;
        map.props.retain(|p| p.asset_id != asset_id);
        for token in &mut map.tokens {
            if token.image_id.as_deref() == Some(asset_id) {
                token.image_id = None;
            }
        }
        store(db, &scene_id, &map)?;
    }
    Ok(())
}
