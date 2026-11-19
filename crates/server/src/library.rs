//! The picture library as a folder the DM can open, as in Foundry: `Library/Tokens/Goblin/
//! goblin.png`, `Library/Props/Chest/chest.png`, `Library/Maps/Goblin cave/map.png`. Each
//! picture keeps its place in the database (`file`, a path inside `Library`), and the DM may
//! change the folder by hand: a scan (at start, and while the host runs) adds what was copied
//! in, follows a renamed folder and drops what was deleted.

use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

pub const LIBRARY: &str = "Library";

/// The kinds the DM may fill by hand; maps belong to tables and are made in the app.
pub const KINDS: [(&str, &str); 4] = [
    ("token", "Tokens"),
    ("prop", "Props"),
    ("handout", "Handouts"),
    ("audio", "Music"),
];
const MAPS: &str = "Maps";

pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join(LIBRARY)
}

fn kind_folder(kind: &str) -> &'static str {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, folder)| *folder)
        .unwrap_or(MAPS)
}

pub fn extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "video/webm" | "audio/webm" => "webm",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/wav" => "wav",
        "audio/mp4" => "m4a",
        _ => "bin",
    }
}

// Names Windows keeps for itself, whatever the extension.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A picture's name as a folder name every computer accepts.
pub fn folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned: String = cleaned
        .trim_matches(|c: char| c == '.' || c == ' ')
        .chars()
        .take(60)
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        "Picture".into()
    } else if RESERVED.contains(&cleaned.to_lowercase().as_str()) {
        format!("{cleaned} picture")
    } else {
        cleaned
    }
}

/// A file name inside a picture's folder: "Ogre Chief" is `ogre-chief`.
fn file_stem(name: &str) -> String {
    let stem: String = folder_name(name)
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let stem = stem
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if stem.is_empty() {
        "picture".into()
    } else {
        stem
    }
}

const MEDIA: [&str; 10] = [
    "png", "jpg", "jpeg", "webp", "gif", "webm", "mp3", "ogg", "flac", "wav",
];

// A picture or a track, by its name; notes or anything else the DM keeps in a folder isn't.
fn is_media(path: &Path) -> bool {
    path.is_file()
        && path.extension().is_some_and(|ext| {
            MEDIA.contains(&ext.to_string_lossy().to_lowercase().as_str()) || ext == "m4a"
        })
}

// How many pictures or tracks a folder holds.
fn media_in(folder: &Path) -> usize {
    std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| is_media(&e.path()) && !junk(&e.file_name().to_string_lossy()))
        .count()
}

/// A path inside the library, or `None` for anything that would step outside it.
pub fn resolve(data_dir: &Path, file: &str) -> Option<PathBuf> {
    let inside = Path::new(file);
    let plain = inside
        .components()
        .all(|part| matches!(part, Component::Normal(_)));
    (plain && !file.is_empty()).then(|| root(data_dir).join(inside))
}

fn relative(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

// Folder names are compared without case: Windows and macOS see "Goblin" and "goblin" as one.
fn taken(parent: &Path, name: &str) -> bool {
    let lower = name.to_lowercase();
    std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().to_lowercase() == lower)
}

/// A new home for a picture: its own folder under its kind, " (2)" and on when the name is
/// taken. Returns the path inside the library; nothing is written yet.
pub fn place(data_dir: &Path, kind: &str, name: &str, mime: &str, stem: Option<&str>) -> String {
    let parent = root(data_dir).join(kind_folder(kind));
    let wanted = folder_name(name);
    let mut folder = wanted.clone();
    let mut n = 2;
    while taken(&parent, &folder) {
        folder = format!("{wanted} ({n})");
        n += 1;
    }
    let file = format!(
        "{}.{}",
        stem.map(str::to_string).unwrap_or_else(|| file_stem(name)),
        extension(mime)
    );
    relative(&Path::new(kind_folder(kind)).join(folder).join(file))
}

/// Writes a picture into the library, whole or not at all.
pub fn put(data_dir: &Path, file: &str, data: &[u8]) -> std::io::Result<()> {
    let path = resolve(data_dir, file).ok_or(std::io::ErrorKind::InvalidInput)?;
    std::fs::create_dir_all(path.parent().expect("a picture's folder"))?;
    let part = path.with_extension("part");
    std::fs::write(&part, data)?;
    std::fs::rename(&part, &path)
}

/// Takes a picture's file away, and its folder once nothing else is in it.
pub fn drop_file(data_dir: &Path, file: &str) {
    let Some(path) = resolve(data_dir, file) else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    if let Some(folder) = path.parent()
        && folder.parent() != Some(&root(data_dir))
        && folder != root(data_dir)
    {
        let _ = std::fs::remove_dir(folder);
    }
}

/// Moves a renamed picture to a folder of its new name, its file renamed to match (a map's
/// stays `map`). A folder holding other pictures too keeps them; only this one moves.
pub fn rehome(data_dir: &Path, file: &str, kind: &str, name: &str, mime: &str) -> Option<String> {
    let stem = if kind == "map" {
        "map".to_string()
    } else {
        file_stem(name)
    };
    let from = resolve(data_dir, file)?;
    let folder = from.parent()?;
    let alone = media_in(folder) == 1;
    let kind_root = root(data_dir).join(kind_folder(kind));
    let own_folder = alone && folder.parent() == Some(kind_root.as_path());
    let wanted = folder_name(name);
    let same_folder = own_folder
        && folder
            .file_name()
            .map(|f| f.to_string_lossy().to_lowercase())
            == Some(wanted.to_lowercase());
    let to = if same_folder {
        relative(
            &Path::new(kind_folder(kind))
                .join(folder.file_name()?)
                .join(format!("{stem}.{}", extension(mime))),
        )
    } else {
        place(data_dir, kind, name, mime, Some(&stem))
    };
    if to == file {
        return Some(to);
    }
    let target = resolve(data_dir, &to)?;
    if own_folder && !same_folder {
        // The whole folder moves, so anything the DM kept beside the picture comes along.
        let new_folder = target.parent()?;
        std::fs::rename(folder, new_folder).ok()?;
        std::fs::rename(new_folder.join(from.file_name()?), &target).ok()?;
    } else {
        std::fs::create_dir_all(target.parent()?).ok()?;
        std::fs::rename(&from, &target).ok()?;
    }
    Some(to)
}

/// Where a map's picture goes: `Maps/<map name>/map.<ext>`.
pub fn place_map(data_dir: &Path, name: &str, mime: &str) -> String {
    place(data_dir, "map", name, mime, Some("map"))
}

// Files the computer or an editor leaves about, never pictures.
fn junk(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.starts_with('.')
        || lower.ends_with(".part")
        || lower.ends_with(".tmp")
        || lower.ends_with('~')
        || matches!(lower.as_str(), "thumbs.db" | "desktop.ini")
}

// Every file under a kind's folder, two folders deep at most.
fn files_under(folder: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if junk(&name) {
            continue;
        }
        if path.is_dir() && depth > 0 {
            files_under(&path, depth - 1, found);
        } else if path.is_file() {
            found.push(path);
        }
    }
}

/// What a scan found: pictures added, renamed or gone, and files it couldn't take, with why.
#[derive(Debug, Default, PartialEq)]
pub struct Scan {
    pub added: Vec<String>,
    pub renamed: Vec<String>,
    pub removed: Vec<String>,
    pub skipped: Vec<String>,
}

impl Scan {
    pub fn changed(&self) -> bool {
        !(self.added.is_empty() && self.renamed.is_empty() && self.removed.is_empty())
    }
}

// The name a file found by hand goes by: its own folder's name, or the file's when it sits
// straight in the kind's folder or shares a folder with others.
fn found_name(kind_root: &Path, path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let Some(folder) = path.parent() else {
        return stem;
    };
    let alone = media_in(folder) == 1;
    if folder != kind_root && alone {
        folder
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or(stem)
    } else {
        stem
    }
}

/// A picture found in the folder, read and checked like an upload.
struct Found {
    kind: &'static str,
    file: String,
    name: String,
    mime: &'static str,
    width: i64,
    height: i64,
    size: i64,
}

fn read_found(data_dir: &Path, kind: &'static str, path: &Path) -> Result<Found, String> {
    use crate::pictures::{
        AUDIO_LIMIT, LIBRARY_LIMIT, VIDEO_LIBRARY_LIMIT, audio_type, picture_type,
    };
    let kind_root = root(data_dir).join(kind_folder(kind));
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    let limit = if kind == "audio" {
        AUDIO_LIMIT
    } else {
        VIDEO_LIBRARY_LIMIT
    };
    if size as usize > limit {
        return Err(format!("over {} MB", limit / (1024 * 1024)));
    }
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let mime = if kind == "audio" {
        audio_type(&data, AUDIO_LIMIT)
    } else {
        picture_type(&data, LIBRARY_LIMIT, VIDEO_LIBRARY_LIMIT)
    }
    .map_err(|e| e.message)?;
    let (width, height) = if kind == "audio" {
        (0, 0)
    } else {
        match imagesize::blob_size(&data) {
            Ok(dim) => (dim.width as i64, dim.height as i64),
            // A clip's size isn't in its first bytes; the map shows it square until resized.
            Err(_) if mime.starts_with("video/") => (512, 512),
            Err(_) => return Err("not a picture this host can read".into()),
        }
    };
    let inside = path
        .strip_prefix(root(data_dir))
        .map_err(|_| "outside the library".to_string())?;
    Ok(Found {
        kind,
        file: relative(inside),
        name: found_name(&kind_root, path)
            .chars()
            .take(60)
            .collect::<String>(),
        mime,
        width,
        height,
        size: size as i64,
    })
}

/// Brings the database in line with the folder. Run with the database held, so the app's own
/// writes (which hold it too) are never half seen. Returns what changed; the caller takes
/// removed pictures off the maps.
pub fn scan(db: &Connection, data_dir: &Path) -> rusqlite::Result<Scan> {
    let mut report = Scan::default();
    let mut rows = db.prepare("SELECT id, kind, file, size FROM assets WHERE file IS NOT NULL")?;
    let known: Vec<(String, String, String, i64)> = rows
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let known_files: HashSet<String> = known.iter().map(|k| k.2.to_lowercase()).collect();
    let mut missing: Vec<&(String, String, String, i64)> = known
        .iter()
        .filter(|(_, _, file, _)| resolve(data_dir, file).is_none_or(|p| !p.is_file()))
        .collect();

    for (kind, folder) in KINDS {
        let mut paths = Vec::new();
        files_under(&root(data_dir).join(folder), 2, &mut paths);
        for path in paths {
            let Ok(inside) = path.strip_prefix(root(data_dir)) else {
                continue;
            };
            if !is_media(&path) || known_files.contains(&relative(inside).to_lowercase()) {
                continue;
            }
            let found = match read_found(data_dir, kind, &path) {
                Ok(found) => found,
                Err(why) => {
                    report.skipped.push(format!("{}: {why}", relative(inside)));
                    continue;
                }
            };
            // A picture that went missing just as one of the same kind and size appeared was
            // moved or renamed by hand: it keeps its id, so the maps keep it.
            let moved = missing.iter().position(|(_, k, file, size)| {
                k == kind
                    && *size == found.size
                    && Path::new(file).extension() == Path::new(&found.file).extension()
            });
            if let Some(at) = moved {
                let (id, ..) = missing.remove(at);
                db.execute(
                    "UPDATE assets SET file = ?2, name = ?3 WHERE id = ?1",
                    params![id, found.file, found.name],
                )?;
                report.renamed.push(found.name);
                continue;
            }
            let id = crate::ids::id();
            db.execute(
                "INSERT INTO assets (id, table_id, kind, name, mime, width, height, created_at, size, file)
                 VALUES (?1, '', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![id, found.kind, found.name, found.mime, found.width, found.height, crate::ids::now(), found.size, found.file],
            )?;
            report.added.push(found.name);
        }
    }
    for (id, ..) in missing {
        let name: Option<String> = db
            .query_row("SELECT name FROM assets WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        db.execute("DELETE FROM assets WHERE id = ?1", [id])?;
        report.removed.push(id.clone());
        if let Some(name) = name {
            tracing::info!("{name} was deleted from the picture library folder");
        }
    }
    Ok(report)
}

/// Scans the folder and brings every table in line: pictures deleted by hand come off the
/// maps, and every screen hears the library changed. Files it couldn't take are kept for the
/// app to show.
pub fn rescan(state: &crate::state::AppState) -> Scan {
    let data_dir = state.config.data_dir.clone();
    let (report, tables) = {
        let db = state.db();
        let report = match scan(&db, &data_dir) {
            Ok(report) => report,
            Err(error) => {
                tracing::error!("scanning the picture library: {error}");
                return Scan::default();
            }
        };
        for id in &report.removed {
            if let Err(error) = crate::pictures::forget_everywhere(&db, id) {
                tracing::error!("taking a deleted picture off the maps: {}", error.message);
            }
        }
        // The library is every table's, so every screen hears it changed.
        let tables: Vec<String> = if report.changed() {
            db.prepare("SELECT id FROM tables")
                .and_then(|mut rows| {
                    rows.query_map([], |row| row.get(0))?
                        .collect::<rusqlite::Result<_>>()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        (report, tables)
    };
    *state.skipped.lock().unwrap_or_else(|p| p.into_inner()) = report.skipped.clone();
    if report.changed() {
        tracing::info!(
            "picture library: {} added, {} renamed, {} removed by hand",
            report.added.len(),
            report.renamed.len(),
            report.removed.len()
        );
        for table in &tables {
            state.events.changed(table);
        }
    }
    report
}

// How long the folder must stay quiet before a scan: copying many files sends many events.
const QUIET: std::time::Duration = std::time::Duration::from_millis(700);

/// Watches the library folder while the host runs, and scans once it goes quiet after a change.
pub fn watch(state: crate::state::AppState) -> tokio::task::JoinHandle<()> {
    use notify::{RecursiveMode, Watcher};
    let library = root(&state.config.data_dir);
    for (_, folder) in KINDS {
        let _ = std::fs::create_dir_all(library.join(folder));
    }
    let (sender, mut changes) = tokio::sync::mpsc::unbounded_channel();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok() {
            let _ = sender.send(());
        }
    });
    tokio::spawn(async move {
        let mut watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                tracing::warn!("can't watch the picture library folder: {error}");
                return;
            }
        };
        if let Err(error) = watcher.watch(&library, RecursiveMode::Recursive) {
            tracing::warn!("can't watch the picture library folder: {error}");
            return;
        }
        while changes.recv().await.is_some() {
            while tokio::time::timeout(QUIET, changes.recv())
                .await
                .is_ok_and(|c| c.is_some())
            {}
            let state = state.clone();
            let _ = tokio::task::spawn_blocking(move || rescan(&state)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_folders_every_computer_accepts() {
        assert_eq!(folder_name("Ogre Chief"), "Ogre Chief");
        assert_eq!(folder_name("Bad/Name: \"x\"?"), "Bad Name x");
        assert_eq!(folder_name("  ..hidden.. "), "hidden");
        assert_eq!(folder_name(""), "Picture");
        assert_eq!(folder_name("CON"), "CON picture");
        assert_eq!(file_stem("Ogre Chief"), "ogre-chief");
        assert_eq!(file_stem("Goblin (2)!"), "goblin-2");
    }

    #[test]
    fn never_steps_outside_the_library() {
        let data = Path::new("/data");
        assert!(resolve(data, "../tables.db").is_none());
        assert!(resolve(data, "/etc/passwd").is_none());
        assert!(resolve(data, "Tokens/../../x").is_none());
        assert!(resolve(data, "").is_none());
        assert_eq!(
            resolve(data, "Tokens/Goblin/goblin.png"),
            Some(PathBuf::from("/data/Library/Tokens/Goblin/goblin.png"))
        );
    }

    #[test]
    fn gives_each_picture_its_own_folder() {
        let data = tempfile::tempdir().unwrap();
        let first = place(data.path(), "token", "Goblin", "image/png", None);
        assert_eq!(first, "Tokens/Goblin/goblin.png");
        put(data.path(), &first, b"x").unwrap();
        assert_eq!(
            place(data.path(), "token", "goblin", "image/webp", None),
            "Tokens/goblin (2)/goblin.webp"
        );
        assert_eq!(
            place_map(data.path(), "Goblin cave", "image/jpeg"),
            "Maps/Goblin cave/map.jpg"
        );
    }
}
