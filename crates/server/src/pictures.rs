//! Pictures: each map's own picture, and the table's library of token portraits, props for the
//! map and handouts to show. They are kept as files in the data folder, one folder per table.

use crate::access::{self, Credentials};
use crate::error::{AppError, Result};
use crate::ids;
use crate::maps;
use crate::state::AppState;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

// Map pictures are big; token portraits and props are small.
pub const MAP_LIMIT: usize = 8 * 1024 * 1024;
pub const LIBRARY_LIMIT: usize = 4 * 1024 * 1024;
// A table's library stays a library, not a photo album.
const MOST_PICTURES: i64 = 200;
const KINDS: [&str; 3] = ["token", "prop", "handout"];

/// The picture's type, told by its first bytes, not by what the upload claims to be.
pub fn picture_type(data: &[u8], limit: usize) -> Result<&'static str> {
    if data.is_empty() {
        return Err(AppError::bad("No picture came with the upload"));
    }
    if data.len() > limit {
        return Err(AppError::bad(format!(
            "The picture is over {} MB",
            limit / (1024 * 1024)
        )));
    }
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Ok("image/png")
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Ok("image/jpeg")
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Ok("image/webp")
    } else {
        Err(AppError::bad("Use a PNG, JPEG or WebP picture"))
    }
}

fn folder(state: &AppState, table_id: &str) -> PathBuf {
    state.config.data_dir.join("pictures").join(table_id)
}

// Ids are made by the host and only ever letters, digits and dashes, so they are safe names.
fn safe(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if ok {
        Ok(name)
    } else {
        Err(AppError::not_found("That picture is gone"))
    }
}

fn map_file(state: &AppState, table_id: &str, scene_id: &str) -> Result<PathBuf> {
    Ok(folder(state, safe(table_id)?).join(format!("{}.map", safe(scene_id)?)))
}

fn library_file(state: &AppState, table_id: &str, asset_id: &str) -> Result<PathBuf> {
    Ok(folder(state, safe(table_id)?).join(format!("{}.pic", safe(asset_id)?)))
}

// Written whole or not at all: a new name first, then moved into place.
fn write(path: &PathBuf, data: &[u8]) -> Result<()> {
    let failed = |error: std::io::Error| {
        tracing::error!("saving a picture: {error}");
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The host couldn't save the picture",
        )
    };
    std::fs::create_dir_all(path.parent().expect("a picture's folder")).map_err(failed)?;
    let part = path.with_extension("part");
    std::fs::write(&part, data).map_err(failed)?;
    std::fs::rename(&part, path).map_err(failed)
}

fn send(path: PathBuf, mime: &str) -> Result<Response> {
    let data = std::fs::read(path).map_err(|_| AppError::not_found("That picture is gone"))?;
    Ok((
        [
            (header::CONTENT_TYPE, mime.to_string()),
            // The address changes when the picture does, so the browser may keep it.
            (
                header::CACHE_CONTROL,
                "private, max-age=31536000, immutable".into(),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
        ],
        data,
    )
        .into_response())
}

/// A map was deleted: its picture goes too.
pub fn forget_map(state: &AppState, table_id: &str, scene_id: &str) {
    if let Ok(path) = map_file(state, table_id, scene_id) {
        let _ = std::fs::remove_file(path);
    }
}

/// A table was deleted: all its pictures go.
pub fn forget_table(state: &AppState, table_id: &str) {
    if safe(table_id).is_ok() {
        let _ = std::fs::remove_dir_all(folder(state, table_id));
    }
}

#[derive(Deserialize)]
pub struct Measured {
    width: u32,
    height: u32,
}

/// `PUT /api/tables/{id}/scenes/{scene}/image`: a new picture for a map, sized as the browser
/// measured it.
pub async fn set_map(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Query(size): Query<Measured>,
    data: Bytes,
) -> Result<StatusCode> {
    access::runner(&state, &id, &Credentials::from(&headers, None, None))?;
    let mime = picture_type(&data, MAP_LIMIT)?;
    if !(100..=16_384).contains(&size.width) || !(100..=16_384).contains(&size.height) {
        return Err(AppError::bad("The picture must be 100 to 16384 pixels"));
    }
    {
        let db = state.db();
        maps::load(&db, &id, &scene)?;
        write(&map_file(&state, &id, &scene)?, &data)?;
        maps::set_picture(&db, &id, &scene, mime, size.width, size.height)?;
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Seen {
    key: Option<String>,
    seat: Option<String>,
}

/// `GET /api/tables/{id}/scenes/{scene}/image`: the map's picture, for anyone at the table.
pub async fn map(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Query(seen): Query<Seen>,
) -> Result<Response> {
    access::at_table(
        &state,
        &id,
        &Credentials::from(&headers, seen.key, seen.seat),
    )?;
    let picture = maps::load(&state.db(), &id, &scene)?
        .image
        .ok_or_else(|| AppError::not_found("This map has no picture"))?;
    send(map_file(&state, &id, &scene)?, &picture.mime)
}

#[derive(Deserialize)]
pub struct Upload {
    kind: String,
    name: Option<String>,
    width: f64,
    height: f64,
}

/// `POST /api/tables/{id}/assets`: a picture for the library. A player may add a token
/// portrait when the DM lets them change tokens; the rest is the DM's.
pub async fn add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(upload): Query<Upload>,
    data: Bytes,
) -> Result<Json<Value>> {
    let access = access::at_table(&state, &id, &Credentials::from(&headers, None, None))?;
    if !KINDS.contains(&upload.kind.as_str()) {
        return Err(AppError::bad("A picture is a token, a prop or a handout"));
    }
    if !(access.runs() || (upload.kind == "token" && access.may("tokens"))) {
        return Err(AppError::not_found("The DM hasn't let you add pictures"));
    }
    let mime = picture_type(&data, LIBRARY_LIMIT)?;
    let fits = |n: f64| n.is_finite() && n > 0.0 && n <= 16_384.0;
    if !fits(upload.width) || !fits(upload.height) {
        return Err(AppError::bad("The picture must be 1 to 16384 pixels"));
    }
    let name: String = upload
        .name
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(60)
        .collect();
    let name = if name.is_empty() {
        "Picture".to_string()
    } else {
        name
    };
    let (width, height) = (upload.width.round() as i64, upload.height.round() as i64);
    let picture_id = ids::id();
    {
        let db = state.db();
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM assets WHERE table_id = ?1",
            [&id],
            |row| row.get(0),
        )?;
        if count >= MOST_PICTURES {
            return Err(AppError::bad(format!(
                "A table keeps {MOST_PICTURES} pictures"
            )));
        }
        write(&library_file(&state, &id, &picture_id)?, &data)?;
        db.execute(
            "INSERT INTO assets (id, table_id, kind, name, mime, width, height, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![picture_id, id, upload.kind, name, mime, width, height, ids::now()],
        )?;
    }
    state.events.changed(&id);
    Ok(Json(
        json!({ "id": picture_id, "kind": upload.kind, "name": name, "width": width, "height": height }),
    ))
}

/// The library as the table view sends it. Players get the token portraits and props on the
/// map; handouts reach them when shown.
pub fn list(db: &rusqlite::Connection, table_id: &str, runs: bool) -> Result<Vec<Value>> {
    let mut rows = db.prepare(
        "SELECT id, kind, name, width, height FROM assets WHERE table_id = ?1 ORDER BY created_at",
    )?;
    let all = rows
        .query_map([table_id], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "kind": row.get::<_, String>(1)?,
                "name": row.get::<_, String>(2)?,
                "width": row.get::<_, i64>(3)?,
                "height": row.get::<_, i64>(4)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(all
        .into_iter()
        .filter(|a| runs || a["kind"] != "handout")
        .collect())
}

/// `GET /api/tables/{id}/assets/{asset}`: a picture from the library, for anyone at the table.
pub async fn picture(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, asset)): Path<(String, String)>,
    Query(seen): Query<Seen>,
) -> Result<Response> {
    access::at_table(
        &state,
        &id,
        &Credentials::from(&headers, seen.key, seen.seat),
    )?;
    let mime: String = state
        .db()
        .query_row(
            "SELECT mime FROM assets WHERE table_id = ?1 AND id = ?2",
            [&id, &asset],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("That picture is gone"))?;
    send(library_file(&state, &id, &asset)?, &mime)
}

/// Deletes a picture, and takes it off every map and the handout.
pub async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, asset)): Path<(String, String)>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &Credentials::from(&headers, None, None))?;
    {
        let db = state.db();
        if db.execute(
            "DELETE FROM assets WHERE table_id = ?1 AND id = ?2",
            [&id, &asset],
        )? == 0
        {
            return Err(AppError::not_found("That picture is gone"));
        }
        maps::forget_picture(&db, &id, &asset)?;
        db.execute("UPDATE tables SET handout = NULL WHERE id = ?1 AND json_extract(handout, '$.assetId') = ?2", [&id, &asset])?;
    }
    let _ = std::fs::remove_file(library_file(&state, &id, &asset)?);
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handout {
    asset_id: String,
    #[serde(default)]
    title: String,
}

/// `PUT /api/tables/{id}/handout`: shows a picture to everyone at the table.
pub async fn show(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(handout): Json<Handout>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &Credentials::from(&headers, None, None))?;
    let title: String = handout.title.trim().chars().take(100).collect();
    {
        let db = state.db();
        let found: i64 = db.query_row(
            "SELECT COUNT(*) FROM assets WHERE table_id = ?1 AND id = ?2",
            [&id, &handout.asset_id],
            |row| row.get(0),
        )?;
        if found == 0 {
            return Err(AppError::not_found("That picture is gone"));
        }
        let shown = json!({ "assetId": handout.asset_id, "title": title }).to_string();
        db.execute(
            "UPDATE tables SET handout = ?2 WHERE id = ?1",
            [&id, &shown],
        )?;
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn hide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &Credentials::from(&headers, None, None))?;
    state
        .db()
        .execute("UPDATE tables SET handout = NULL WHERE id = ?1", [&id])?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_a_picture_by_its_first_bytes() {
        assert_eq!(
            picture_type(b"\x89PNG\r\n\x1a\nrest", 100).unwrap(),
            "image/png"
        );
        assert_eq!(
            picture_type(b"\xff\xd8\xffrest", 100).unwrap(),
            "image/jpeg"
        );
        assert_eq!(
            picture_type(b"RIFF\0\0\0\0WEBPrest", 100).unwrap(),
            "image/webp"
        );
        assert!(picture_type(b"<svg onload=alert(1)>", 100).is_err());
        assert!(picture_type(&[0xff, 0xd8, 0xff, 0, 0], 3).is_err());
    }

    #[test]
    fn never_names_a_file_outside_the_table() {
        assert!(safe("../../etc/passwd").is_err());
        assert!(safe("abc/def").is_err());
        assert!(safe("tok-AbC123").is_ok());
    }
}
