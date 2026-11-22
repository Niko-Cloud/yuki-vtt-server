//! A board (a scene): map pictures laid on it under everything else, with the board's own grid
//! (the website's `packages/engine/src/board.ts`).

use crate::access;
use crate::error::{AppError, Result};
use crate::ids;
use crate::maps::{TableMap, credentials, edit, load, store};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use yuki_vtt_rules::map::MapGrid;

pub const MOST_BOARD: f64 = 16_384.0;
pub const MOST_MAP_ITEMS: usize = 50;

/// A map picture on a board: a library picture of kind "map", placed and sized in the board's
/// pixels, in quarter turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapItem {
    pub id: String,
    pub asset: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
}

/// A map picture's own grid, as the DM lined it up: pixels per square and where lines start.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PictureGrid {
    pub cell: f64,
    pub offset_x: f64,
    pub offset_y: f64,
}

/// How big a picture is drawn on a board so its squares match the board's.
pub fn item_size(
    width: f64,
    height: f64,
    grid: Option<&PictureGrid>,
    board_cell: f64,
) -> (f64, f64) {
    let scale = board_cell / grid.map_or(board_cell, |g| g.cell);
    (width * scale, height * scale)
}

/// Where an item reaches, as turned.
pub fn item_box(item: &MapItem) -> (f64, f64) {
    if matches!(item.rotation, Some(90 | 270)) {
        (item.height, item.width)
    } else {
        (item.width, item.height)
    }
}

/// A board grown, never shrunk, to hold its map pictures.
pub fn grown(width: u32, height: u32, maps: &[MapItem]) -> (u32, u32) {
    let (mut w, mut h) = (f64::from(width), f64::from(height));
    for item in maps {
        let (across, down) = item_box(item);
        w = w.max((item.x + across).ceil());
        h = h.max((item.y + down).ceil());
    }
    (w.min(MOST_BOARD) as u32, h.min(MOST_BOARD) as u32)
}

/// The spot an item goes to so its picture's squares sit on the board's.
pub fn snap(x: f64, y: f64, board: &MapGrid, grid: Option<&PictureGrid>, scale: f64) -> (f64, f64) {
    let cell = f64::from(board.size.max(1));
    let one = |value: f64, own: f64, offset: u32| {
        let offset = f64::from(offset);
        ((value + own - offset) / cell).round() * cell - own + offset
    };
    let (ox, oy) = grid.map_or((0.0, 0.0), |g| (g.offset_x * scale, g.offset_y * scale));
    (
        one(x, ox, board.offset_x).max(0.0),
        one(y, oy, board.offset_y).max(0.0),
    )
}

// A map picture of the library: its size and own grid. Only a map picture goes on a board.
fn map_picture(db: &Connection, asset: &str) -> Result<(f64, f64, Option<PictureGrid>)> {
    let found: Option<(String, i64, i64, Option<String>)> = db
        .query_row(
            "SELECT kind, width, height, grid FROM assets WHERE id = ?1",
            [asset],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let (kind, width, height, grid) =
        found.ok_or_else(|| AppError::not_found("That picture is gone"))?;
    if kind != "map" {
        return Err(AppError::bad("Only a map picture goes on a board"));
    }
    let grid = grid.and_then(|text| serde_json::from_str(&text).ok());
    Ok((width as f64, height as f64, grid))
}

fn grow(mut map: TableMap) -> TableMap {
    (map.width, map.height) = grown(map.width, map.height, &map.maps);
    map
}

#[derive(Deserialize)]
pub struct NewItem {
    asset: String,
    x: f64,
    y: f64,
}

/// `POST /api/tables/{id}/scenes/{scene}/maps`: a map picture laid on a board.
pub async fn add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewItem>,
) -> Result<Json<Value>> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    if !(0.0..=MOST_BOARD).contains(&body.x) || !(0.0..=MOST_BOARD).contains(&body.y) {
        return Err(AppError::bad("A map goes on the board"));
    }
    let picture = map_picture(&state.db(), &body.asset)?;
    let made = format!("map-{}", ids::id());
    edit(&state, &access, &id, &scene, |mut map| {
        if map.maps.len() >= MOST_MAP_ITEMS {
            return Err(AppError::bad(format!(
                "A board holds {MOST_MAP_ITEMS} maps"
            )));
        }
        let cell = f64::from(map.grid.size.max(1));
        let (width, height) = item_size(picture.0, picture.1, picture.2.as_ref(), cell);
        let scale = width / picture.0;
        let (x, y) = snap(body.x, body.y, &map.grid, picture.2.as_ref(), scale);
        map.maps.push(MapItem {
            id: made.clone(),
            asset: body.asset,
            x,
            y,
            width,
            height,
            rotation: None,
            locked: Some(true),
        });
        Ok(grow(map))
    })?;
    Ok(Json(json!({ "id": made })))
}

#[derive(Deserialize)]
pub struct ItemChange {
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
    rotation: Option<u32>,
    locked: Option<bool>,
    layer: Option<String>,
}

pub async fn change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, item_id)): Path<(String, String, String)>,
    Json(body): Json<ItemChange>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    let on_board = |n: Option<f64>, least: f64| n.is_none_or(|n| (least..=MOST_BOARD).contains(&n));
    if !(on_board(body.x, 0.0) && on_board(body.y, 0.0))
        || !(on_board(body.width, 16.0) && on_board(body.height, 16.0))
    {
        return Err(AppError::bad("A map goes on the board"));
    }
    if body
        .rotation
        .is_some_and(|r| !matches!(r, 0 | 90 | 180 | 270))
    {
        return Err(AppError::bad("A map turns in quarter turns"));
    }
    if body
        .layer
        .as_deref()
        .is_some_and(|l| l != "front" && l != "back")
    {
        return Err(AppError::bad("A map goes to the front or the back"));
    }
    edit(&state, &access, &id, &scene, |mut map| {
        let at = map
            .maps
            .iter()
            .position(|m| m.id == item_id)
            .ok_or_else(|| AppError::not_found("That map is gone"))?;
        let mut item = map.maps.remove(at);
        let moving = body.x.is_some()
            || body.y.is_some()
            || body.width.is_some()
            || body.height.is_some()
            || body.rotation.is_some();
        if moving && item.locked == Some(true) && body.locked != Some(false) {
            return Err(AppError::bad("That map is locked"));
        }
        item.x = body.x.unwrap_or(item.x);
        item.y = body.y.unwrap_or(item.y);
        item.width = body.width.unwrap_or(item.width);
        item.height = body.height.unwrap_or(item.height);
        if let Some(rotation) = body.rotation {
            item.rotation = (rotation != 0).then_some(rotation);
        }
        if let Some(locked) = body.locked {
            item.locked = locked.then_some(true);
        }
        match body.layer.as_deref() {
            Some("front") => map.maps.push(item),
            Some("back") => map.maps.insert(0, item),
            _ => map.maps.insert(at, item),
        }
        Ok(grow(map))
    })
}

pub async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, item_id)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    edit(&state, &access, &id, &scene, |mut map| {
        map.maps.retain(|m| m.id != item_id);
        Ok(map)
    })
}

/// A map picture's grid was lined up anew: its items on every board are resized to match.
pub fn refit(db: &Connection, asset: &str) -> Result<()> {
    let (width, height, grid) = map_picture(db, asset)?;
    let scenes: Vec<(String, String)> = db
        .prepare("SELECT table_id, id FROM scenes")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (table, scene) in scenes {
        let mut map = load(db, &table, &scene)?;
        if !map.maps.iter().any(|m| m.asset == asset) {
            continue;
        }
        let cell = f64::from(map.grid.size.max(1));
        let (w, h) = item_size(width, height, grid.as_ref(), cell);
        for item in map.maps.iter_mut().filter(|m| m.asset == asset) {
            (item.width, item.height) = (w, h);
        }
        store(db, &scene, &grow(map))?;
    }
    Ok(())
}

// What a map kept before boards: its own picture, and other maps laid on it as pieces.
#[derive(Deserialize)]
struct OldMap {
    image: Option<OldPicture>,
    #[serde(default)]
    pieces: Vec<OldPiece>,
}

#[derive(Deserialize)]
struct OldPicture {
    mime: String,
}

#[derive(Deserialize)]
struct OldPiece {
    map: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    rotation: Option<u32>,
    locked: Option<bool>,
}

/// Turns maps made before boards into boards, once, at start: a map's own picture (already in
/// `Library/Maps`) becomes a map picture in the library, laid on its board where it was, with
/// the map's grid as its own; pieces of other maps become map pictures on the board too. Each
/// map turns in one transaction, and a map already turned is left alone. The number changed.
pub fn convert_old_maps(data_dir: &std::path::Path, db: &Connection) -> Result<usize> {
    // The old fields, read from what is stored before anything is turned (a map's own picture
    // and its pieces are no part of a board, so storing it again drops them).
    let stored: Vec<(String, String, String)> = db
        .prepare("SELECT table_id, id, data FROM scenes")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut mime_of = std::collections::HashMap::new();
    let mut pieces_of = Vec::new();
    for (table_id, scene_id, data) in stored {
        let Ok(old) = serde_json::from_str::<OldMap>(&data) else {
            continue;
        };
        if let Some(image) = old.image {
            mime_of.insert(scene_id.clone(), image.mime);
        }
        if !old.pieces.is_empty() {
            pieces_of.push((table_id, scene_id, old.pieces));
        }
    }
    let found: Vec<(String, String, String, String, Option<String>)> = db
        .prepare("SELECT table_id, id, name, file, folder FROM scenes WHERE file IS NOT NULL")?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut picture_of = std::collections::HashMap::new();
    let mut done = 0;
    for (table_id, scene_id, name, file, folder) in found {
        let Ok(map) = load(db, &table_id, &scene_id) else {
            continue;
        };
        let Some(mime) = mime_of.get(&scene_id) else {
            continue;
        };
        let size = crate::library::resolve(data_dir, &file)
            .and_then(|path| std::fs::metadata(path).ok())
            .map_or(0, |meta| meta.len() as i64);
        let picture = ids::id();
        let grid = PictureGrid {
            cell: map.grid.size as f64,
            offset_x: map.grid.offset_x as f64,
            offset_y: map.grid.offset_y as f64,
        };
        let item = MapItem {
            id: format!("map-{}", ids::id()),
            asset: picture.clone(),
            x: 0.0,
            y: 0.0,
            width: map.width as f64,
            height: map.height as f64,
            rotation: None,
            locked: Some(true),
        };
        let mut maps = vec![item];
        maps.extend(map.maps.iter().cloned());
        let turned = TableMap { maps, ..map };
        let tx = db.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO assets (id, table_id, kind, name, mime, width, height, created_at, size, file, folder, grid)
             VALUES (?1, ?2, 'map', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                picture,
                table_id,
                name,
                mime,
                turned.width,
                turned.height,
                ids::now(),
                size,
                file,
                folder,
                serde_json::to_string(&grid).expect("a grid"),
            ],
        )?;
        store(&tx, &scene_id, &turned)?;
        tx.execute("UPDATE scenes SET file = NULL WHERE id = ?1", [&scene_id])?;
        tx.commit()?;
        picture_of.insert(scene_id, picture);
        done += 1;
    }
    // Then the pieces: each a map picture on the board, as many squares across as before.
    for (table_id, scene_id, pieces) in pieces_of {
        let Ok(map) = load(db, &table_id, &scene_id) else {
            continue;
        };
        let cell = map.grid.size as f64;
        let (left, top) = (map.grid.offset_x as f64, map.grid.offset_y as f64);
        let mut maps = map.maps.clone();
        for piece in &pieces {
            // A map turned on an earlier start: the picture its board shows first.
            let asset = picture_of.get(&piece.map).cloned().or_else(|| {
                load(db, &table_id, &piece.map)
                    .ok()
                    .and_then(|source| source.maps.first().map(|m| m.asset.clone()))
            });
            let Some(asset) = asset else {
                continue;
            };
            maps.push(MapItem {
                id: format!("map-{}", ids::id()),
                asset,
                x: left + piece.x * cell,
                y: top + piece.y * cell,
                width: piece.w * cell,
                height: piece.h * cell,
                rotation: piece.rotation.filter(|turn| *turn != 0),
                locked: piece.locked.filter(|locked| *locked),
            });
        }
        store(db, &scene_id, &TableMap { maps, ..map })?;
        done += 1;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_grows_like_the_website() {
        let grid = PictureGrid {
            cell: 50.0,
            offset_x: 0.0,
            offset_y: 0.0,
        };
        assert_eq!(item_size(1000.0, 500.0, Some(&grid), 70.0), (1400.0, 700.0));
        assert_eq!(item_size(1000.0, 500.0, None, 70.0), (1000.0, 500.0));
        let item = MapItem {
            id: "m".into(),
            asset: "a".into(),
            x: 1400.0,
            y: 0.0,
            width: 700.0,
            height: 980.0,
            rotation: None,
            locked: None,
        };
        assert_eq!(grown(1400, 980, std::slice::from_ref(&item)), (2100, 980));
        assert_eq!(grown(3000, 980, std::slice::from_ref(&item)), (3000, 980));
        let turned = MapItem {
            rotation: Some(90),
            ..item
        };
        assert_eq!(grown(100, 100, &[turned]), (2380, 700));
    }
}
