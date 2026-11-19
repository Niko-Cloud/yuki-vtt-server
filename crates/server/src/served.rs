//! The copy of a still picture that players get. The DM's own file in the library stays exactly
//! as it is; what goes out over the DM's upload is a WebP no wider than its kind needs, with
//! nothing a camera wrote into it (where a photo was taken). Pictures uploaded through the
//! browser already are that and go as they are; this is for files copied into the folder by
//! hand, and for older uploads. Copies are kept in `cache/`, so each is made once.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

// How far across a still is kept, as the browser keeps it (`apps/web/src/room/measure.ts`).
fn longest_side(kind: &str) -> u32 {
    match kind {
        "token" => 1024,
        "prop" => 2048,
        _ => 4096,
    }
}
const QUALITY: f32 = 82.0;

fn cache(data_dir: &Path) -> PathBuf {
    data_dir.join("cache")
}

/// Whether a picture carries notes a camera or an editor wrote into it, which may say where
/// a photo was taken.
pub fn has_notes(data: &[u8]) -> bool {
    if data.starts_with(&[0xff, 0xd8]) {
        // JPEG: an APP1 (EXIF, XMP) or APP13 (Photoshop) segment before the picture itself.
        let mut at = 2;
        while at + 4 <= data.len() && data[at] == 0xff {
            let marker = data[at + 1];
            if marker == 0xe1 || marker == 0xed {
                return true;
            }
            if marker == 0xda {
                break;
            }
            let length = u16::from_be_bytes([data[at + 2], data[at + 3]]) as usize;
            at += 2 + length;
        }
        return false;
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut at = 8;
        while at + 8 <= data.len() {
            let length = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
            let kind = &data[at + 4..at + 8];
            if matches!(kind, b"eXIf" | b"tEXt" | b"iTXt" | b"zTXt") {
                return true;
            }
            if kind == b"IDAT" {
                break;
            }
            at += 12 + length;
        }
        return false;
    }
    if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        let mut at = 12;
        while at + 8 <= data.len() {
            let kind = &data[at..at + 4];
            let length = u32::from_le_bytes(data[at + 4..at + 8].try_into().unwrap()) as usize;
            if matches!(kind, b"EXIF" | b"XMP ") {
                return true;
            }
            at += 8 + length + (length & 1);
        }
    }
    false
}

/// The file to send for a picture, and its type: the DM's file when it is already a clean WebP
/// small enough, otherwise a cleaned, smaller copy. Anything this can't read goes as it is.
pub fn served(data_dir: &Path, id: &str, path: &Path, kind: &str, mime: &str) -> (PathBuf, String) {
    let as_is = (path.to_path_buf(), mime.to_string());
    if !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
        return as_is;
    }
    let Ok(data) = std::fs::read(path) else {
        return as_is;
    };
    let side = longest_side(kind);
    let Ok(size) = imagesize::blob_size(&data) else {
        return as_is;
    };
    let too_wide = size.width.max(size.height) > side as usize;
    if mime == "image/webp" && !too_wide && !has_notes(&data) {
        return as_is;
    }
    // Named by the file it came from, so a new picture makes a new copy and the old one goes.
    let stamp = hex::encode(Sha256::digest(&data));
    let copy = cache(data_dir).join(format!("{id}-{}-{side}.webp", &stamp[..16]));
    if copy.is_file() {
        return (copy, "image/webp".into());
    }
    match make_copy(&data, side) {
        Some(webp) if webp.len() < data.len() || has_notes(&data) || too_wide => {
            forget(data_dir, id);
            let part = copy.with_extension("part");
            let written = std::fs::create_dir_all(cache(data_dir))
                .and_then(|()| std::fs::write(&part, &webp))
                .and_then(|()| std::fs::rename(&part, &copy));
            match written {
                Ok(()) => (copy, "image/webp".into()),
                Err(error) => {
                    tracing::warn!("couldn't keep a smaller copy of a picture: {error}");
                    as_is
                }
            }
        }
        _ => as_is,
    }
}

fn make_copy(data: &[u8], side: u32) -> Option<Vec<u8>> {
    let picture = image::load_from_memory(data).ok()?;
    let picture = if picture.width().max(picture.height()) > side {
        picture.resize(side, side, image::imageops::FilterType::Lanczos3)
    } else {
        picture
    };
    let rgba = picture.to_rgba8();
    let encoder = webp::Encoder::from_rgba(&rgba, rgba.width(), rgba.height());
    Some(encoder.encode(QUALITY).to_vec())
}

/// Takes away the kept copies of a picture or map that changed or went.
pub fn forget(data_dir: &Path, id: &str) {
    let prefix = format!("{id}-");
    for entry in std::fs::read_dir(cache(data_dir))
        .into_iter()
        .flatten()
        .flatten()
    {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, note: bool) -> Vec<u8> {
        let picture = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(picture)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        if note {
            // A text chunk after the header, as a camera app might add.
            let text = b"tEXtComment\0taken at 51.5N 0.12W";
            let mut chunk = (text.len() as u32 - 4).to_be_bytes().to_vec();
            chunk.extend_from_slice(text);
            chunk.extend_from_slice(&[0, 0, 0, 0]);
            out.splice(33..33, chunk);
        }
        out
    }

    #[test]
    fn tells_a_picture_with_notes_in_it() {
        assert!(!has_notes(&png(4, 4, false)));
        assert!(has_notes(&png(4, 4, true)));
        let jpeg_with_exif = [0xff, 0xd8, 0xff, 0xe1, 0x00, 0x04, 0x45, 0x78, 0xff, 0xda];
        assert!(has_notes(&jpeg_with_exif));
        let plain_jpeg = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x4a, 0x46, 0xff, 0xda];
        assert!(!has_notes(&plain_jpeg));
    }

    #[test]
    fn sends_a_small_clean_webp_and_keeps_the_dms_file() {
        let data = tempfile::tempdir().unwrap();
        let file = data.path().join("big.png");
        let original = png(3000, 1500, true);
        std::fs::write(&file, &original).unwrap();

        let (sent, mime) = served(data.path(), "pic-1", &file, "token", "image/png");
        assert_eq!(mime, "image/webp");
        let copy = std::fs::read(&sent).unwrap();
        assert!(copy.len() < original.len());
        assert!(!has_notes(&copy));
        let size = imagesize::blob_size(&copy).unwrap();
        assert_eq!((size.width, size.height), (1024, 512));
        assert_eq!(
            std::fs::read(&file).unwrap(),
            original,
            "the DM's file is untouched"
        );
        // Made once: the second time it is the same copy.
        assert_eq!(
            served(data.path(), "pic-1", &file, "token", "image/png").0,
            sent
        );
        // A clip or a track goes as it is.
        let clip = served(data.path(), "pic-2", &file, "token", "video/webm");
        assert_eq!(clip, (file.clone(), "video/webm".to_string()));
        forget(data.path(), "pic-1");
        assert!(!sent.exists());
    }
}
