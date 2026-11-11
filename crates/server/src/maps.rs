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
use yuki_vtt_rules::map::{self, Cell, MapGrid};

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
    /// The cell of its top-left corner.
    pub x: u32,
    pub y: u32,
    /// Cells across: 1 for Medium and smaller, 2 Large, 3 Huge, 4 Gargantuan.
    pub size: u32,
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
}

/// A picture laid on the map that isn't a token: a door, a chest, a cart. Sized in cells.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prop {
    pub id: String,
    pub asset_id: String,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
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
}

/// What the DM sets for a map: its size and grid.
#[derive(Debug, Clone, Copy, Deserialize)]
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
        if ok {
            Ok(())
        } else {
            Err(AppError::bad(
                "A map is 100 to 16384 pixels, with cells of 8 to 400",
            ))
        }
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
        },
        image: None,
        tokens: vec![],
        props: vec![],
        drawings: vec![],
    }
}

impl TableMap {
    fn cells(&self) -> (u32, u32) {
        map::cells(self.width, self.height, &self.grid)
    }

    // A token stays whole on the map, also after the map shrinks.
    fn keep_token(&self, mut token: Token) -> Token {
        let cell = map::keep_inside(
            self.width,
            self.height,
            &self.grid,
            Cell {
                x: token.x,
                y: token.y,
            },
            token.size,
        );
        (token.x, token.y) = (cell.x, cell.y);
        token
    }

    fn keep_prop(&self, mut prop: Prop) -> Prop {
        let (columns, rows) = self.cells();
        prop.w = prop.w.min(columns);
        prop.h = prop.h.min(rows);
        prop.x = prop.x.min(columns - prop.w);
        prop.y = prop.y.min(rows - prop.h);
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

fn credentials(headers: &HeaderMap) -> Credentials {
    Credentials::from(headers, None)
}

fn not_allowed(what: &str) -> AppError {
    AppError::not_found(format!("The DM hasn't let you {what}"))
}

fn allow(allowed: bool, what: &str) -> Result<()> {
    if allowed {
        Ok(())
    } else {
        Err(not_allowed(what))
    }
}

fn color(value: &str) -> Result<String> {
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

fn cell(x: u32, y: u32) -> Result<()> {
    if x > 1000 || y > 1000 {
        Err(AppError::bad("That's off the map"))
    } else {
        Ok(())
    }
}

fn size(size: u32) -> Result<u32> {
    if (1..=4).contains(&size) {
        Ok(size)
    } else {
        Err(AppError::bad("A token is 1 to 4 cells across"))
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
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT name, data FROM scenes WHERE table_id = ?1 AND id = ?2",
            [table_id, scene_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((name, data)) = row else {
        return Ok(None);
    };
    let mut scene: Value = serde_json::from_str(&data)?;
    scene["id"] = json!(scene_id);
    scene["name"] = json!(name);
    Ok(Some(scene))
}

/// Every scene, by name, in order; for whoever runs the table.
pub fn list(db: &Connection, table_id: &str) -> Result<Vec<Value>> {
    let mut rows = db
        .prepare("SELECT id, name FROM scenes WHERE table_id = ?1 ORDER BY position, created_at")?;
    let scenes = rows
        .query_map([table_id], |row| {
            Ok(json!({ "id": row.get::<_, String>(0)?, "name": row.get::<_, String>(1)? }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(scenes)
}

fn active_scene(db: &Connection, table_id: &str) -> Result<Option<String>> {
    Ok(db.query_row(
        "SELECT active_scene_id FROM tables WHERE id = ?1",
        [table_id],
        |row| row.get(0),
    )?)
}

// Changes a map. Players act on the map everyone sees, never on the DM's others.
fn edit(
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
}

fn scene_name(value: &str) -> Result<String> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(AppError::bad("A map's name is 1 to 60 letters"));
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
            "INSERT INTO scenes (id, table_id, name, position, data, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![scene_id, id, name, count, serde_json::to_string(&new_map)?, ids::now()],
        )?;
        db.execute(
            "UPDATE tables SET active_scene_id = ?2 WHERE id = ?1 AND active_scene_id IS NULL",
            [&id, &scene_id],
        )?;
    }
    state.events.changed(&id);
    Ok(Json(json!({ "id": scene_id, "name": name })))
}

#[derive(Deserialize)]
pub struct SceneChange {
    name: Option<String>,
    setup: Option<Setup>,
}

/// Renames a map, or changes its size and grid; tokens stay whole on it.
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
    x: u32,
    y: u32,
    #[serde(default = "one")]
    size: u32,
    color: Option<String>,
    image_id: Option<String>,
}

fn one() -> u32 {
    1
}

pub async fn add_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewToken>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("tokens"), "add tokens")?;
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
        hidden: None,
        locked: None,
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
                x: i as u32,
                y: 0,
                size: 1,
                color: TOKEN_COLORS[0].into(),
                seat_id: Some(seat),
                image_id: None,
                hidden: None,
                locked: None,
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
    x: Option<u32>,
    y: Option<u32>,
    name: Option<String>,
    size: Option<u32>,
    color: Option<String>,
    /// A picture's id, or null to take the picture off.
    #[serde(default, deserialize_with = "present")]
    image_id: Option<Value>,
    hidden: Option<bool>,
    locked: Option<bool>,
}

// A field that was sent, null included: Option alone reads null as not sent, so "take the
// picture off" would change nothing. A field left out stays None.
fn present<'de, D: serde::Deserializer<'de>>(
    sent: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(sent).map(Some)
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
        let moving = body.x.is_some() || body.y.is_some();
        let restyling =
            body.name.is_some() || body.size.is_some() || body.color.is_some() || picture.is_some();
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
        if let Some(name) = &body.name {
            token.name = token_name(name)?;
        }
        if let Some(new_size) = body.size {
            token.size = size(new_size)?;
        }
        if let Some(new_color) = &body.color {
            token.color = color(new_color)?;
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
        map.tokens[at] = map.keep_token(token);
        Ok(map)
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
        map.tokens
            .retain(|t| t.id != token_id || (!access.runs() && t.hidden == Some(true)));
        Ok(map)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProp {
    asset_id: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    locked: Option<bool>,
}

fn prop_size(w: u32, h: u32) -> Result<()> {
    if (1..=40).contains(&w) && (1..=40).contains(&h) {
        Ok(())
    } else {
        Err(AppError::bad("A picture on the map is 1 to 40 cells"))
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
    cell(body.x, body.y)?;
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
        });
        map.props.push(prop);
        Ok(map)
    })
}

#[derive(Deserialize)]
pub struct PropChange {
    x: Option<u32>,
    y: Option<u32>,
    w: Option<u32>,
    h: Option<u32>,
    locked: Option<bool>,
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
        cell(prop.x, prop.y)?;
        prop_size(prop.w, prop.h)?;
        if let Some(locked) = body.locked {
            prop.locked = locked.then_some(true);
        }
        map.props[at] = map.keep_prop(prop);
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
}

fn pen() -> String {
    "pen".into()
}

pub async fn add_drawing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewDrawing>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("draw"), "draw")?;
    if !["pen", "line", "rect", "circle"].contains(&body.shape.as_str()) {
        return Err(AppError::bad(
            "A drawing is a pen line, a line, a square or a circle",
        ));
    }
    if !(2..=MOST_POINTS).contains(&body.points.len())
        || body.points.iter().flatten().any(|n| !n.is_finite())
    {
        return Err(AppError::bad(format!(
            "A drawing has 2 to {MOST_POINTS} points"
        )));
    }
    let by = if access.runs() {
        "dm".to_string()
    } else {
        access
            .seat_ids
            .first()
            .cloned()
            .unwrap_or_else(|| "guest".into())
    };
    let drawing = Drawing {
        id: format!("draw-{}", ids::id()),
        shape: body.shape,
        color: color(&body.color)?,
        points: body.points,
        by,
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
    state.events.ping(
        &id,
        json!({ "sceneId": scene, "x": at.x, "y": at.y, "by": access.name, "color": color }),
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
