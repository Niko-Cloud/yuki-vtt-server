//! The picture library as a folder the DM can open, as in Foundry: `Library/Tokens/Goblin/
//! goblin.png`, `Library/Props/Chest/chest.png`, `Library/Maps/Goblin cave/map.png`. Each
//! picture keeps its place in the database (`file`, a path inside `Library`).

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
