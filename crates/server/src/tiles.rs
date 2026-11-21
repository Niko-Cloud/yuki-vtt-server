//! A big map picture in square tiles at several levels, so a screen fetches only the part it
//! looks at, as sharp as it is zoomed. The same rules as the website's (`packages/engine/src/
//! map-tiles.ts`): tiles 1024 pixels across; level 0 the whole picture, each level after half
//! as wide and tall, until one tile holds it all; only pictures wider or taller than 4096.
//! Cut from the DM's own file the first time one is asked for, all at once, and kept in
//! `cache/` beside the other copies.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const TILE: u32 = 1024;
pub const TILE_FROM: u32 = 4096;
const QUALITY: f32 = 80.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tiles {
    pub width: u32,
    pub height: u32,
    pub levels: u32,
}

/// A picture's tiles, or none when it goes whole (small enough, or a clip).
pub fn tiles_for(width: u32, height: u32, mime: &str) -> Option<Tiles> {
    if mime.starts_with("video/") {
        return None;
    }
    let side = width.max(height);
    if side <= TILE_FROM {
        return None;
    }
    let (mut levels, mut at) = (1, side);
    while at > TILE {
        at = at.div_ceil(2);
        levels += 1;
    }
    Some(Tiles {
        width,
        height,
        levels,
    })
}

/// How big the picture is at a level.
pub fn level_size(tiles: &Tiles, level: u32) -> (u32, u32) {
    let scale = 1 << level;
    (tiles.width.div_ceil(scale), tiles.height.div_ceil(scale))
}

/// A tile's name ("level-column-row") read back, when it is one of this picture's.
pub fn parse_key(tiles: &Tiles, key: &str) -> Option<(u32, u32, u32)> {
    let mut parts = key.split('-');
    let level = parts.next()?;
    let x = parts.next()?;
    let y = parts.next()?;
    let digits = |part: &str, most: usize| {
        (!part.is_empty() && part.len() <= most && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<u32>().ok())
            .flatten()
    };
    let (level, x, y) = (digits(level, 1)?, digits(x, 3)?, digits(y, 3)?);
    if parts.next().is_some() || level >= tiles.levels {
        return None;
    }
    let (width, height) = level_size(tiles, level);
    (x < width.div_ceil(TILE) && y < height.div_ceil(TILE)).then_some((level, x, y))
}

// One picture is cut at a time: a whole map in memory is big enough.
static CUTTING: Mutex<()> = Mutex::new(());

/// The file of one tile, cut (with all the others) the first time. None when the picture
/// can't be read.
pub fn tile(data_dir: &Path, id: &str, path: &Path, tiles: &Tiles, key: &str) -> Option<PathBuf> {
    parse_key(tiles, key)?;
    // Named by the file's size and when it changed, so a new picture gets new tiles.
    let meta = std::fs::metadata(path).ok()?;
    let changed = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    let prefix = format!("{id}-t{:x}{changed:x}", meta.len());
    let dir = data_dir.join("cache");
    let named = |key: &str| dir.join(format!("{prefix}-{key}.webp"));
    if named(key).is_file() {
        return Some(named(key));
    }
    let _one = CUTTING.lock().unwrap_or_else(|p| p.into_inner());
    if named(key).is_file() {
        return Some(named(key));
    }
    std::fs::create_dir_all(&dir).ok()?;
    let mut level = image::open(path).ok()?.to_rgba8();
    for at in 0..tiles.levels {
        let (width, height) = level_size(tiles, at);
        if level.dimensions() != (width, height) {
            level = image::imageops::resize(
                &level,
                width,
                height,
                image::imageops::FilterType::Triangle,
            );
        }
        for y in 0..height.div_ceil(TILE) {
            for x in 0..width.div_ceil(TILE) {
                let (left, top) = (x * TILE, y * TILE);
                let piece = image::imageops::crop_imm(
                    &level,
                    left,
                    top,
                    TILE.min(width - left),
                    TILE.min(height - top),
                )
                .to_image();
                let webp = webp::Encoder::from_rgba(&piece, piece.width(), piece.height())
                    .encode(QUALITY)
                    .to_vec();
                let file = named(&format!("{at}-{x}-{y}"));
                let part = file.with_extension("part");
                std::fs::write(&part, &webp).ok()?;
                std::fs::rename(&part, &file).ok()?;
            }
        }
    }
    named(key).is_file().then(|| named(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_only_big_pictures_into_levels_like_the_website() {
        assert_eq!(tiles_for(4096, 3000, "image/webp"), None);
        assert_eq!(tiles_for(8000, 8000, "video/mp4"), None);
        assert_eq!(
            tiles_for(8192, 5000, "image/jpeg"),
            Some(Tiles {
                width: 8192,
                height: 5000,
                levels: 4
            })
        );
        assert_eq!(tiles_for(5000, 9000, "image/png").unwrap().levels, 5);
        let tiles = tiles_for(5000, 3000, "image/png").unwrap();
        assert_eq!(tiles.levels, 4);
        assert_eq!(parse_key(&tiles, "0-4-2"), Some((0, 4, 2)));
        assert_eq!(parse_key(&tiles, "0-5-0"), None);
        assert_eq!(parse_key(&tiles, "4-0-0"), None);
        assert_eq!(parse_key(&tiles, "../x"), None);
        assert_eq!(parse_key(&tiles, "0-1-1-1"), None);
    }

    #[test]
    fn cuts_every_tile_once_short_at_the_edges() {
        let dir = tempfile::tempdir().unwrap();
        let picture = image::RgbaImage::from_fn(4100, 1200, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 9, 255])
        });
        let path = dir.path().join("map.png");
        picture.save(&path).unwrap();
        let tiles = tiles_for(4100, 1200, "image/png").unwrap();
        assert_eq!(tiles.levels, 4);
        let last = tile(dir.path(), "scene", &path, &tiles, "0-4-1").unwrap();
        let cut = image::open(&last).unwrap();
        assert_eq!((cut.width(), cut.height()), (4, 176));
        let top = tile(dir.path(), "scene", &path, &tiles, "3-0-0").unwrap();
        let cut = image::open(&top).unwrap();
        assert_eq!((cut.width(), cut.height()), (513, 150));
        // All 10 + 3 + 2 + 1 at once.
        let kept = std::fs::read_dir(dir.path().join("cache")).unwrap().count();
        assert_eq!(kept, 16);
        assert!(tile(dir.path(), "scene", &path, &tiles, "0-9-0").is_none());
    }
}
