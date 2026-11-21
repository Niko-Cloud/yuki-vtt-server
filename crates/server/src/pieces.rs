//! Other maps' pictures laid on a map as pieces of one bigger board (the website's map
//! pieces): at a cell, as many cells across and down as their own map has, so the grids line
//! up, in quarter turns. A blank map grows to hold them; a map with a picture keeps them on it.

use crate::access;
use crate::error::{AppError, Result};
use crate::ids;
use crate::maps::{Image, TableMap, credentials, edit, load};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

pub const MOST_PIECES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Piece {
    pub id: String,
    /// The map whose picture it is.
    pub map: String,
    pub image: Image,
    pub x: u32,
    pub y: u32,
    pub w: f64,
    pub h: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
}

impl Piece {
    fn turned(&self) -> bool {
        matches!(self.rotation, Some(90 | 270))
    }
    fn across(&self) -> f64 {
        if self.turned() { self.h } else { self.w }
    }
    fn down(&self) -> f64 {
        if self.turned() { self.w } else { self.h }
    }
}

// As many cells as the map it comes from has, to a hundredth.
fn size_of(source: &TableMap) -> (f64, f64) {
    let cells = |pixels: u32| {
        ((f64::from(pixels) / f64::from(source.grid.size.max(1))) * 100.0).round() / 100.0
    };
    (cells(source.width).max(1.0), cells(source.height).max(1.0))
}

/// A map's pieces kept on it; a blank map grows to hold them, as far as a map may.
pub fn fit(mut map: TableMap) -> TableMap {
    let cell = f64::from(map.grid.size.max(1));
    if map.image.is_none() {
        for piece in &map.pieces {
            let edge = |at: u32, cells: f64, offset: u32| {
                ((f64::from(at) + cells) * cell + f64::from(offset))
                    .ceil()
                    .min(16_384.0) as u32
            };
            map.width = map
                .width
                .max(edge(piece.x, piece.across(), map.grid.offset_x));
            map.height = map
                .height
                .max(edge(piece.y, piece.down(), map.grid.offset_y));
        }
    }
    let (columns, rows) = yuki_vtt_rules::map::cells(map.width, map.height, &map.grid);
    let keep =
        |at: u32, cells: f64, most: u32| at.min((f64::from(most) - cells).floor().max(0.0) as u32);
    for piece in &mut map.pieces {
        piece.x = keep(piece.x, piece.across(), columns);
        piece.y = keep(piece.y, piece.down(), rows);
    }
    map
}

#[derive(Deserialize)]
pub struct NewPiece {
    from: String,
    x: u32,
    y: u32,
}

/// `POST /api/tables/{id}/scenes/{scene}/pieces`: another map's picture laid on this one.
pub async fn add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewPiece>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    if body.from == scene {
        return Err(AppError::bad("A map can't be a piece of itself"));
    }
    if body.x > 1000 || body.y > 1000 {
        return Err(AppError::bad("A piece goes on the map"));
    }
    let source = load(&state.db(), &id, &body.from)?;
    let image = source
        .image
        .clone()
        .ok_or_else(|| AppError::bad("That map has no picture"))?;
    let (w, h) = size_of(&source);
    edit(&state, &access, &id, &scene, |mut map| {
        if map.pieces.len() >= MOST_PIECES {
            return Err(AppError::bad(format!("A map holds {MOST_PIECES} pieces")));
        }
        map.pieces.push(Piece {
            id: format!("piece-{}", ids::id()),
            map: body.from,
            image,
            x: body.x,
            y: body.y,
            w,
            h,
            rotation: None,
            locked: None,
        });
        Ok(fit(map))
    })
}

#[derive(Deserialize)]
pub struct PieceChange {
    x: Option<u32>,
    y: Option<u32>,
    rotation: Option<u32>,
    locked: Option<bool>,
    layer: Option<String>,
}

pub async fn change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, piece_id)): Path<(String, String, String)>,
    Json(body): Json<PieceChange>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    if body
        .rotation
        .is_some_and(|r| !matches!(r, 0 | 90 | 180 | 270))
    {
        return Err(AppError::bad("A piece turns in quarter turns"));
    }
    if body.x.is_some_and(|x| x > 1000) || body.y.is_some_and(|y| y > 1000) {
        return Err(AppError::bad("A piece goes on the map"));
    }
    if body
        .layer
        .as_deref()
        .is_some_and(|l| l != "front" && l != "back")
    {
        return Err(AppError::bad("A piece goes to the front or the back"));
    }
    edit(&state, &access, &id, &scene, |mut map| {
        let at = map
            .pieces
            .iter()
            .position(|p| p.id == piece_id)
            .ok_or_else(|| AppError::not_found("That piece is gone"))?;
        let mut piece = map.pieces.remove(at);
        let moving = body.x.is_some() || body.y.is_some() || body.rotation.is_some();
        if moving && piece.locked == Some(true) && body.locked != Some(false) {
            return Err(AppError::bad("That piece is locked"));
        }
        piece.x = body.x.unwrap_or(piece.x);
        piece.y = body.y.unwrap_or(piece.y);
        if let Some(rotation) = body.rotation {
            piece.rotation = (rotation != 0).then_some(rotation);
        }
        if let Some(locked) = body.locked {
            piece.locked = locked.then_some(true);
        }
        match body.layer.as_deref() {
            Some("front") => map.pieces.push(piece),
            Some("back") => map.pieces.insert(0, piece),
            _ => map.pieces.insert(at, piece),
        }
        Ok(fit(map))
    })
}

pub async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, piece_id)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    edit(&state, &access, &id, &scene, |mut map| {
        map.pieces.retain(|p| p.id != piece_id);
        Ok(map)
    })
}

/// A map's picture changed, or the map went: the pieces showing it on the table's other maps
/// show the new picture at its size, or go.
pub fn follow_source(db: &Connection, table_id: &str, source_id: &str) -> Result<()> {
    let source = load(db, table_id, source_id).ok();
    let size = source.as_ref().map(size_of);
    let image = source.and_then(|s| s.image);
    let scenes: Vec<String> = db
        .prepare("SELECT id FROM scenes WHERE table_id = ?1")?
        .query_map([table_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for scene in scenes {
        let mut map = load(db, table_id, &scene)?;
        if !map.pieces.iter().any(|p| p.map == source_id) {
            continue;
        }
        map.pieces = std::mem::take(&mut map.pieces)
            .into_iter()
            .filter_map(|mut piece| {
                if piece.map != source_id {
                    return Some(piece);
                }
                piece.image = image.clone()?;
                (piece.w, piece.h) = size?;
                Some(piece)
            })
            .collect();
        crate::maps::store(db, &scene, &fit(map))?;
    }
    Ok(())
}
