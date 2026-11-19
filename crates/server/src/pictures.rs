//! Pictures: each map's own picture, and the host's library of token portraits, props for the
//! map, handouts to show and music. They are kept as files in the data folder: map pictures in
//! one folder per table, the library in one folder that every table shares.

use crate::access::{self, Credentials, Role};
use crate::error::{AppError, Result};
use crate::ids;
use crate::library;
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
use std::path::{Path as FilePath, PathBuf};

// Map pictures are big; token portraits and props are small.
pub const MAP_LIMIT: usize = 8 * 1024 * 1024;
pub const LIBRARY_LIMIT: usize = 4 * 1024 * 1024;
// Animated pictures (GIF and WebM video) may be larger: a short looping clip weighs more.
pub const VIDEO_MAP_LIMIT: usize = 25 * 1024 * 1024;
pub const VIDEO_LIBRARY_LIMIT: usize = 10 * 1024 * 1024;
// A music track, compressed in the browser before it goes up, may weigh as much as a clip.
pub const AUDIO_LIMIT: usize = 10 * 1024 * 1024;
// The host's library stays a library, not a photo album.
const MOST_PICTURES: i64 = 1000;
const KINDS: [&str; 4] = ["token", "prop", "handout", "audio"];

/// The picture's type, told by its first bytes, not by what the upload claims to be.
pub fn picture_type(data: &[u8], limit: usize, animated_limit: usize) -> Result<&'static str> {
    if data.is_empty() {
        return Err(AppError::bad("No picture came with the upload"));
    }
    let mime = if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        "image/png"
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "image/webp"
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        "image/gif"
    } else if data.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        "video/webm"
    } else {
        return Err(AppError::bad("Use a PNG, JPEG, WebP, GIF or WebM picture"));
    };
    let animated = mime == "image/gif" || mime.starts_with("video/");
    let cap = if animated { animated_limit } else { limit };
    if data.len() > cap {
        return Err(AppError::bad(format!(
            "The picture is over {} MB",
            cap / (1024 * 1024)
        )));
    }
    Ok(mime)
}

/// A music track's type, told by its first bytes. MP3 opens with an ID3 tag or a frame sync;
/// the rest have their own marks. A WebM track shares the EBML header with a clip.
pub fn audio_type(data: &[u8], limit: usize) -> Result<&'static str> {
    if data.is_empty() {
        return Err(AppError::bad("No track came with the upload"));
    }
    let mime = if data.starts_with(b"ID3")
        || (data.len() >= 2 && data[0] == 0xff && data[1] & 0xe0 == 0xe0)
    {
        "audio/mpeg"
    } else if data.starts_with(b"OggS") {
        "audio/ogg"
    } else if data.starts_with(b"fLaC") {
        "audio/flac"
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WAVE" {
        "audio/wav"
    } else if data.len() >= 12 && &data[4..8] == b"ftyp" {
        "audio/mp4"
    } else if data.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        "audio/webm"
    } else {
        return Err(AppError::bad(
            "Use an MP3, OGG, M4A, WAV, FLAC or WebM track",
        ));
    };
    if data.len() > limit {
        return Err(AppError::bad(format!(
            "The track is over {} MB",
            limit / (1024 * 1024)
        )));
    }
    Ok(mime)
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

fn not_saved(error: std::io::Error) -> AppError {
    tracing::error!("saving a picture: {error}");
    AppError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "The host couldn't save the picture",
    )
}

/// Where a library picture is kept, inside the library folder.
fn library_path(db: &rusqlite::Connection, data_dir: &FilePath, asset_id: &str) -> Result<PathBuf> {
    let file: Option<String> = db
        .query_row("SELECT file FROM assets WHERE id = ?1", [asset_id], |row| {
            row.get(0)
        })
        .optional()?
        .flatten();
    file.and_then(|file| library::resolve(data_dir, &file))
        .ok_or_else(|| AppError::not_found("That picture is gone"))
}

/// The file of a map's picture, inside the library folder, if it has one.
pub fn map_file(
    db: &rusqlite::Connection,
    table_id: &str,
    scene_id: &str,
) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT file FROM scenes WHERE table_id = ?1 AND id = ?2",
            [table_id, scene_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Brings pictures kept the old way (by id, beside their table or in `library/`) into the
/// readable library folder, and notes the size of any picture that lacks one. Runs at start;
/// a picture already in place is left alone.
pub fn gather_library(data_dir: &FilePath, db: &rusqlite::Connection) -> rusqlite::Result<()> {
    let mut rows =
        db.prepare("SELECT id, table_id, kind, name, mime, size FROM assets WHERE file IS NULL")?;
    let pictures = rows
        .query_map([], |row| {
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
    for (asset_id, table_id, kind, name, mime, size) in pictures {
        let Ok(asset_id) = safe(&asset_id) else {
            continue;
        };
        let mut old = vec![data_dir.join("library").join(format!("{asset_id}.pic"))];
        if let Ok(table_id) = safe(&table_id) {
            old.push(
                data_dir
                    .join("pictures")
                    .join(table_id)
                    .join(format!("{asset_id}.pic")),
            );
        }
        let Some(old) = old.into_iter().find(|path| path.is_file()) else {
            continue;
        };
        let file = library::place(data_dir, &kind, &name, &mime, None);
        if let Some(new) = library::resolve(data_dir, &file) {
            let moved = std::fs::create_dir_all(new.parent().expect("a picture's folder"))
                .and_then(|()| std::fs::rename(&old, &new));
            match moved {
                Ok(()) => {
                    let size = if size > 0 {
                        size
                    } else {
                        std::fs::metadata(&new).map(|m| m.len() as i64).unwrap_or(0)
                    };
                    db.execute(
                        "UPDATE assets SET file = ?2, size = ?3 WHERE id = ?1",
                        params![asset_id, file, size],
                    )?;
                }
                Err(error) => tracing::error!("moving a picture into the library: {error}"),
            }
        }
    }
    let mut rows = db.prepare("SELECT id, table_id, name, data FROM scenes WHERE file IS NULL")?;
    let maps = rows
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (scene_id, table_id, name, data) in maps {
        let (Ok(scene_id), Ok(table_id)) = (safe(&scene_id), safe(&table_id)) else {
            continue;
        };
        let old = data_dir
            .join("pictures")
            .join(table_id)
            .join(format!("{scene_id}.map"));
        let mime = serde_json::from_str::<Value>(&data)
            .ok()
            .and_then(|map| map["image"]["mime"].as_str().map(str::to_string));
        let (true, Some(mime)) = (old.is_file(), mime) else {
            continue;
        };
        let file = library::place_map(data_dir, &name, &mime);
        if let Some(new) = library::resolve(data_dir, &file) {
            let moved = std::fs::create_dir_all(new.parent().expect("a map's folder"))
                .and_then(|()| std::fs::rename(&old, &new));
            match moved {
                Ok(()) => {
                    db.execute(
                        "UPDATE scenes SET file = ?2 WHERE id = ?1",
                        params![scene_id, file],
                    )?;
                }
                Err(error) => tracing::error!("moving a map picture into the library: {error}"),
            }
        }
    }
    // The old folders go once nothing is left in them.
    let _ = std::fs::remove_dir(data_dir.join("library"));
    for entry in std::fs::read_dir(data_dir.join("pictures"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let _ = std::fs::remove_dir(entry.path());
    }
    let _ = std::fs::remove_dir(data_dir.join("pictures"));
    Ok(())
}

/// The kind of a picture in the library, if it is there.
pub fn kind_of(db: &rusqlite::Connection, asset_id: &str) -> Result<String> {
    db.query_row("SELECT kind FROM assets WHERE id = ?1", [asset_id], |row| {
        row.get(0)
    })
    .optional()?
    .ok_or_else(|| AppError::not_found("That picture is gone"))
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

// What players get of a still: a small, clean copy where the DM's own file isn't one. Making
// the copy the first time takes a moment, so it runs beside the requests, not in their way.
async fn send_served(
    state: &AppState,
    id: String,
    path: PathBuf,
    kind: &str,
    mime: String,
) -> Result<Response> {
    let data_dir = state.config.data_dir.clone();
    let kind = kind.to_string();
    let (path, mime) = tokio::task::spawn_blocking(move || {
        crate::served::served(&data_dir, &id, &path, &kind, &mime)
    })
    .await
    .map_err(|_| {
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The picture couldn't be read",
        )
    })?;
    send(path, &mime)
}

/// A map was deleted: its picture goes too.
pub fn forget_map(state: &AppState, file: Option<String>) {
    if let Some(file) = file {
        library::drop_file(&state.config.data_dir, &file);
    }
}

/// A map was copied: the copy gets its own copy of the picture, under its own name.
pub fn copy_map(state: &AppState, file: Option<&str>, name: &str) -> Result<Option<String>> {
    let Some(file) = file else {
        return Ok(None);
    };
    let data_dir = &state.config.data_dir;
    let from = library::resolve(data_dir, file)
        .ok_or_else(|| AppError::not_found("This map's picture couldn't be read"))?;
    let data = std::fs::read(&from)
        .map_err(|_| AppError::not_found("This map's picture couldn't be read"))?;
    let mime = picture_type(&data, MAP_LIMIT, VIDEO_MAP_LIMIT)?;
    let copy = library::place_map(data_dir, name, mime);
    library::put(data_dir, &copy, &data).map_err(not_saved)?;
    Ok(Some(copy))
}

/// A renamed map's picture moves to a folder of its new name.
pub fn rename_map(
    state: &AppState,
    db: &rusqlite::Connection,
    scene_id: &str,
    file: &str,
    name: &str,
    mime: &str,
) -> Result<()> {
    if let Some(moved) = library::rehome(&state.config.data_dir, file, "map", name, mime) {
        db.execute(
            "UPDATE scenes SET file = ?2 WHERE id = ?1",
            params![scene_id, moved],
        )?;
    }
    Ok(())
}

/// A table was deleted: its map pictures go. The library keeps what was uploaded there.
pub fn forget_table(state: &AppState, files: Vec<String>) {
    for file in files {
        library::drop_file(&state.config.data_dir, &file);
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
    access::runner(&state, &id, &Credentials::from(&headers, None))?;
    let mime = picture_type(&data, MAP_LIMIT, VIDEO_MAP_LIMIT)?;
    if !(100..=16_384).contains(&size.width) || !(100..=16_384).contains(&size.height) {
        return Err(AppError::bad("The picture must be 100 to 16384 pixels"));
    }
    {
        let db = state.db();
        maps::load(&db, &id, &scene)?;
        let name: String = db.query_row(
            "SELECT name FROM scenes WHERE table_id = ?1 AND id = ?2",
            [&id, &scene],
            |row| row.get(0),
        )?;
        // A new picture of the same type replaces the old in its folder; another type gets
        // its own file, and the old one goes.
        let had = map_file(&db, &id, &scene)?;
        let data_dir = &state.config.data_dir;
        let file = match &had {
            Some(file) if file.ends_with(&format!(".{}", library::extension(mime))) => file.clone(),
            _ => library::place_map(data_dir, &name, mime),
        };
        library::put(data_dir, &file, &data).map_err(not_saved)?;
        if let Some(old) = had.filter(|old| *old != file) {
            library::drop_file(data_dir, &old);
        }
        crate::served::forget(data_dir, &scene);
        db.execute(
            "UPDATE scenes SET file = ?3 WHERE table_id = ?1 AND id = ?2",
            params![id, scene, file],
        )?;
        maps::set_picture(&db, &id, &scene, mime, size.width, size.height)?;
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Seen {
    pass: Option<String>,
}

/// `GET /api/tables/{id}/scenes/{scene}/image`: the map's picture, for anyone at the table.
pub async fn map(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Query(seen): Query<Seen>,
) -> Result<Response> {
    access::at_table(&state, &id, &Credentials::from(&headers, seen.pass))?;
    let (picture, file) = {
        let db = state.db();
        let picture = maps::load(&db, &id, &scene)?
            .image
            .ok_or_else(|| AppError::not_found("This map has no picture"))?;
        (picture, map_file(&db, &id, &scene)?)
    };
    let path = file
        .and_then(|file| library::resolve(&state.config.data_dir, &file))
        .ok_or_else(|| AppError::not_found("This map has no picture"))?;
    send_served(&state, scene, path, "map", picture.mime).await
}

#[derive(Deserialize)]
pub struct Upload {
    kind: String,
    name: Option<String>,
    width: f64,
    height: f64,
}

/// `POST /api/tables/{id}/assets`: a picture for the host's library, noted as uploaded at this
/// table. A player may add a token portrait when the DM lets them change tokens; the rest is
/// the DM's.
pub async fn add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(upload): Query<Upload>,
    data: Bytes,
) -> Result<Json<Value>> {
    let access = access::at_table(&state, &id, &Credentials::from(&headers, None))?;
    if !KINDS.contains(&upload.kind.as_str()) {
        return Err(AppError::bad(
            "A picture is a token, a prop, a handout or a track",
        ));
    }
    if !(access.runs() || (upload.kind == "token" && access.may("tokens"))) {
        return Err(AppError::not_found("The DM hasn't let you add pictures"));
    }
    let audio = upload.kind == "audio";
    let mime = if audio {
        audio_type(&data, AUDIO_LIMIT)?
    } else {
        picture_type(&data, LIBRARY_LIMIT, VIDEO_LIBRARY_LIMIT)?
    };
    // A track has no pixels; only a picture is measured.
    let fits = |n: f64| n.is_finite() && n > 0.0 && n <= 16_384.0;
    if !audio && (!fits(upload.width) || !fits(upload.height)) {
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
    let (width, height) = if audio {
        (0, 0)
    } else {
        (upload.width.round() as i64, upload.height.round() as i64)
    };
    let picture_id = ids::id();
    {
        let db = state.db();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM assets", [], |row| row.get(0))?;
        if count >= MOST_PICTURES {
            return Err(AppError::bad(format!(
                "A library keeps {MOST_PICTURES} pictures"
            )));
        }
        // Written with the database held, so a scan of the folder never sees it half added.
        let data_dir = &state.config.data_dir;
        let file = library::place(data_dir, &upload.kind, &name, mime, None);
        library::put(data_dir, &file, &data).map_err(not_saved)?;
        db.execute(
            "INSERT INTO assets (id, table_id, kind, name, mime, width, height, created_at, size, file) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![picture_id, id, upload.kind, name, mime, width, height, ids::now(), data.len() as i64, file],
        )?;
    }
    state.events.changed(&id);
    Ok(Json(
        json!({ "id": picture_id, "kind": upload.kind, "name": name, "width": width, "height": height, "mime": mime, "here": true, "folder": null, "tags": [], "favorite": false, "size": data.len() }),
    ))
}

/// The library as the table view sends it, with `here` on what was uploaded at this table.
/// Whoever runs the table sees all of it. A player gets the token portraits and props uploaded
/// here or on the map they see (`shown`); handouts reach them when shown.
pub fn list(
    db: &rusqlite::Connection,
    table_id: &str,
    runs: bool,
    shown: Option<&Value>,
) -> Result<Vec<Value>> {
    let mut rows = db.prepare(&format!("SELECT {SHOWN} FROM assets ORDER BY created_at"))?;
    let all = rows
        .query_map([], |row| shown_row(row, table_id))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if runs {
        return Ok(all);
    }
    let on_map = shown.map(pictures_on).unwrap_or_default();
    Ok(all
        .into_iter()
        .filter(|a| a["kind"] == "token" || a["kind"] == "prop")
        .filter(|a| a["here"] == true || a["id"].as_str().is_some_and(|id| on_map.contains(&id)))
        .collect())
}

// The columns a table is sent about a picture, read by `shown_row`.
const SHOWN: &str = "id, kind, name, width, height, mime, table_id, folder, tags, favorite, size";

fn shown_row(row: &rusqlite::Row, table_id: &str) -> rusqlite::Result<Value> {
    let tags: String = row.get(8)?;
    Ok(json!({
        "id": row.get::<_, String>(0)?,
        "kind": row.get::<_, String>(1)?,
        "name": row.get::<_, String>(2)?,
        "width": row.get::<_, i64>(3)?,
        "height": row.get::<_, i64>(4)?,
        "mime": row.get::<_, String>(5)?,
        "here": row.get::<_, String>(6)? == table_id,
        "folder": row.get::<_, Option<String>>(7)?,
        "tags": serde_json::from_str::<Vec<String>>(&tags).unwrap_or_default(),
        "favorite": row.get::<_, bool>(9)?,
        "size": row.get::<_, i64>(10)?,
    }))
}

// The library pictures a map shows: its props and token portraits.
fn pictures_on(scene: &Value) -> Vec<&str> {
    let field = |list: &str, key: &str| {
        scene[list]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item[key].as_str())
            .collect::<Vec<_>>()
    };
    [field("props", "assetId"), field("tokens", "imageId")].concat()
}

/// `GET /api/tables/{id}/assets/{asset}`: a picture from the library, for anyone at the table.
pub async fn picture(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, asset)): Path<(String, String)>,
    Query(seen): Query<Seen>,
) -> Result<Response> {
    access::at_table(&state, &id, &Credentials::from(&headers, seen.pass))?;
    let (kind, mime, path) = {
        let db = state.db();
        let (kind, mime): (String, String) = db
            .query_row(
                "SELECT kind, mime FROM assets WHERE id = ?1",
                [&asset],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("That picture is gone"))?;
        (
            kind,
            mime,
            library_path(&db, &state.config.data_dir, &asset)?,
        )
    };
    send_served(&state, asset, path, &kind, mime).await
}

/// Takes a deleted picture off every map, handout, track and background, and says which tables
/// it was at.
pub fn forget_everywhere(db: &rusqlite::Connection, asset_id: &str) -> Result<Vec<String>> {
    let mut rows = db.prepare("SELECT id FROM tables")?;
    let tables = rows
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for table in &tables {
        maps::forget_picture(db, table, asset_id)?;
    }
    for column in ["handout", "music", "background"] {
        db.execute(
            &format!(
                "UPDATE tables SET {column} = NULL WHERE json_extract({column}, '$.assetId') = ?1"
            ),
            [asset_id],
        )?;
    }
    Ok(tables)
}

// A co-DM looks after what was uploaded at their table; the DM looks after the whole library.
fn may_keep(access: &access::Access, table_id: &str, uploaded_at: &str) -> bool {
    access.role == Role::Dm || uploaded_at == table_id
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    name: Option<String>,
    /// A folder's name, or null to take the picture out of its folder.
    #[serde(default, deserialize_with = "crate::play::present")]
    folder: Option<Option<String>>,
    tags: Option<Vec<String>>,
    favorite: Option<bool>,
}

fn trimmed(text: &str, most: usize, what: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > most {
        return Err(AppError::bad(format!("{what} is 1 to {most} letters")));
    }
    Ok(text.to_string())
}

/// `PATCH /api/tables/{id}/assets/{asset}`: renames a picture, files it in a folder, tags it
/// or stars it, for every table on the host.
pub async fn change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, asset)): Path<(String, String)>,
    Json(body): Json<Change>,
) -> Result<Json<Value>> {
    let access = access::runner(&state, &id, &Credentials::from(&headers, None))?;
    let name = body
        .name
        .as_deref()
        .map(|name| trimmed(name, 60, "A name"))
        .transpose()?;
    let folder = body
        .folder
        .map(|folder| {
            folder
                .as_deref()
                .map(|name| trimmed(name, 40, "A folder's name"))
                .transpose()
        })
        .transpose()?;
    let tags = match body.tags {
        Some(tags) if tags.len() > 10 => return Err(AppError::bad("A picture keeps 10 tags")),
        Some(tags) => {
            let mut kept: Vec<String> = Vec::new();
            for tag in tags {
                let tag = trimmed(&tag, 24, "A tag")?.to_lowercase();
                if !kept.contains(&tag) {
                    kept.push(tag);
                }
            }
            Some(serde_json::to_string(&kept)?)
        }
        None => None,
    };
    let (changed, tables) = {
        let db = state.db();
        let from: String = db
            .query_row(
                "SELECT table_id FROM assets WHERE id = ?1",
                [&asset],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("That picture is gone"))?;
        if !may_keep(&access, &id, &from) {
            return Err(AppError::forbidden(
                "Only the DM changes pictures from their other tables",
            ));
        }
        if let Some(name) = &name {
            db.execute(
                "UPDATE assets SET name = ?2 WHERE id = ?1",
                params![asset, name],
            )?;
            // The picture's folder takes the new name, so the library folder reads the same.
            let (kind, mime, file): (String, String, Option<String>) = db.query_row(
                "SELECT kind, mime, file FROM assets WHERE id = ?1",
                [&asset],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if let Some(moved) = file
                .and_then(|file| library::rehome(&state.config.data_dir, &file, &kind, name, &mime))
            {
                db.execute(
                    "UPDATE assets SET file = ?2 WHERE id = ?1",
                    params![asset, moved],
                )?;
            }
        }
        if let Some(folder) = &folder {
            db.execute(
                "UPDATE assets SET folder = ?2 WHERE id = ?1",
                params![asset, folder],
            )?;
        }
        if let Some(tags) = &tags {
            db.execute(
                "UPDATE assets SET tags = ?2 WHERE id = ?1",
                params![asset, tags],
            )?;
        }
        if let Some(favorite) = body.favorite {
            db.execute(
                "UPDATE assets SET favorite = ?2 WHERE id = ?1",
                params![asset, favorite],
            )?;
        }
        let changed = db.query_row(
            &format!("SELECT {SHOWN} FROM assets WHERE id = ?1"),
            [&asset],
            |row| shown_row(row, &id),
        )?;
        let mut rows = db.prepare("SELECT id FROM tables")?;
        let tables = rows
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        (changed, tables)
    };
    for table in &tables {
        state.events.changed(table);
    }
    Ok(Json(changed))
}

/// Deletes a picture, and takes it off every map, handout, track and background of every table
/// on the host. A co-DM only deletes what was uploaded at their table.
pub async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, asset)): Path<(String, String)>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &Credentials::from(&headers, None))?;
    let tables = {
        let db = state.db();
        let from: String = db
            .query_row(
                "SELECT table_id FROM assets WHERE id = ?1",
                [&asset],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("That picture is gone"))?;
        if !may_keep(&access, &id, &from) {
            return Err(AppError::forbidden(
                "Only the DM deletes pictures from their other tables",
            ));
        }
        let file: Option<String> =
            db.query_row("SELECT file FROM assets WHERE id = ?1", [&asset], |row| {
                row.get(0)
            })?;
        db.execute("DELETE FROM assets WHERE id = ?1", [&asset])?;
        if let Some(file) = file {
            library::drop_file(&state.config.data_dir, &file);
        }
        crate::served::forget(&state.config.data_dir, &asset);
        forget_everywhere(&db, &asset)?
    };
    for table in &tables {
        state.events.changed(table);
    }
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
    access::runner(&state, &id, &Credentials::from(&headers, None))?;
    let title: String = handout.title.trim().chars().take(100).collect();
    {
        let db = state.db();
        kind_of(&db, &handout.asset_id)?;
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
    access::runner(&state, &id, &Credentials::from(&headers, None))?;
    state
        .db()
        .execute("UPDATE tables SET handout = NULL WHERE id = ?1", [&id])?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Music {
    asset_id: String,
    #[serde(default)]
    name: String,
}

/// `PUT /api/tables/{id}/music`: plays a track to everyone at the table, looping.
pub async fn play_music(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(music): Json<Music>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &Credentials::from(&headers, None))?;
    let name: String = music.name.trim().chars().take(80).collect();
    {
        let db = state.db();
        kind_of(&db, &music.asset_id)?;
        let playing = json!({ "assetId": music.asset_id, "name": name }).to_string();
        db.execute(
            "UPDATE tables SET music = ?2 WHERE id = ?1",
            [&id, &playing],
        )?;
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/tables/{id}/music`: stops the track for everyone.
pub async fn stop_music(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &Credentials::from(&headers, None))?;
    state
        .db()
        .execute("UPDATE tables SET music = NULL WHERE id = ?1", [&id])?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Background {
    asset_id: String,
}

/// `PUT /api/tables/{id}/background`: puts a picture behind the map for everyone.
pub async fn set_background(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(background): Json<Background>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &Credentials::from(&headers, None))?;
    {
        let db = state.db();
        if kind_of(&db, &background.asset_id)? == "audio" {
            return Err(AppError::bad("A background is a picture, not a track"));
        }
        let shown = json!({ "assetId": background.asset_id }).to_string();
        db.execute(
            "UPDATE tables SET background = ?2 WHERE id = ?1",
            [&id, &shown],
        )?;
    }
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/tables/{id}/background`: takes the picture away from everyone.
pub async fn clear_background(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    access::runner(&state, &id, &Credentials::from(&headers, None))?;
    state
        .db()
        .execute("UPDATE tables SET background = NULL WHERE id = ?1", [&id])?;
    state.events.changed(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_a_picture_by_its_first_bytes() {
        assert_eq!(
            picture_type(b"\x89PNG\r\n\x1a\nrest", 100, 100).unwrap(),
            "image/png"
        );
        assert_eq!(
            picture_type(b"\xff\xd8\xffrest", 100, 100).unwrap(),
            "image/jpeg"
        );
        assert_eq!(
            picture_type(b"RIFF\0\0\0\0WEBPrest", 100, 100).unwrap(),
            "image/webp"
        );
        assert_eq!(picture_type(b"GIF89arest", 100, 100).unwrap(), "image/gif");
        assert_eq!(
            picture_type(b"\x1a\x45\xdf\xa3rest", 100, 100).unwrap(),
            "video/webm"
        );
        assert!(picture_type(b"<svg onload=alert(1)>", 100, 100).is_err());
        assert!(picture_type(&[0xff, 0xd8, 0xff, 0, 0], 3, 3).is_err());
        // An animated picture takes the larger cap, a still one the smaller.
        assert!(picture_type(b"GIF89arest", 3, 100).is_ok());
        assert!(picture_type(b"\x89PNG\r\n\x1a\nrest", 3, 100).is_err());
    }

    #[test]
    fn never_names_a_file_outside_the_table() {
        assert!(safe("../../etc/passwd").is_err());
        assert!(safe("abc/def").is_err());
        assert!(safe("tok-AbC123").is_ok());
    }

    #[test]
    fn tells_a_track_by_its_first_bytes() {
        assert_eq!(audio_type(b"ID3rest", 100).unwrap(), "audio/mpeg");
        assert_eq!(
            audio_type(&[0xff, 0xfb, 0x90, 0x00], 100).unwrap(),
            "audio/mpeg"
        );
        assert_eq!(audio_type(b"OggSrest", 100).unwrap(), "audio/ogg");
        assert_eq!(audio_type(b"fLaCrest", 100).unwrap(), "audio/flac");
        assert_eq!(
            audio_type(b"RIFF\0\0\0\0WAVErest", 100).unwrap(),
            "audio/wav"
        );
        assert_eq!(
            audio_type(b"\0\0\0\x18ftypM4A rest", 100).unwrap(),
            "audio/mp4"
        );
        assert_eq!(
            audio_type(b"\x1a\x45\xdf\xa3rest", 100).unwrap(),
            "audio/webm"
        );
        assert!(audio_type(b"<svg onload=alert(1)>", 100).is_err());
        assert!(audio_type(b"ID3rest", 3).is_err());
    }
}
