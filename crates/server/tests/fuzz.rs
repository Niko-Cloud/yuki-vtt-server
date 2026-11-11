//! Throws random and broken input at everything that reads what strangers send: links,
//! signed characters, pictures, backups and the routes. Nothing may panic or answer 500.
//! Slow, so it runs on demand: `cargo test --test fuzz -- --ignored`.

mod common;

use axum::http::StatusCode;
use common::*;
use rand::{Rng, SeedableRng, rngs::StdRng};
use serde_json::{Value, json};
use yuki_vtt_server::snapshot::Signed;
use yuki_vtt_server::{backup, pictures, website};

const ROUNDS: usize = 20_000;

// Bits that make parsers take odd paths: escapes, separators, numbers at the edges.
const PIECES: &[&str] = &[
    "%",
    "%2",
    "%zz",
    "%00",
    "%ff",
    "+",
    "&",
    "=",
    "?",
    "/",
    "://",
    "yukivtt://",
    "pair",
    "start",
    "website=",
    "token=",
    "https://",
    "http://",
    "[",
    "]",
    ":",
    ".",
    "..",
    "\u{0}",
    "\u{1f409}",
    "é",
    "-1",
    "9999999999999999999999",
    "null",
    "{",
    "}",
    "\"",
];

fn noise(rng: &mut StdRng, most: usize) -> String {
    let mut text = String::new();
    for _ in 0..rng.random_range(0..most) {
        if rng.random_bool(0.6) {
            text.push_str(PIECES[rng.random_range(0..PIECES.len())]);
        } else {
            text.push(rng.random_range(' '..='~'));
        }
    }
    text
}

fn bytes(rng: &mut StdRng, most: usize) -> Vec<u8> {
    (0..rng.random_range(0..most))
        .map(|_| rng.random())
        .collect()
}

// A JSON value of any shape, a few levels deep.
fn any_json(rng: &mut StdRng, depth: u32) -> Value {
    match rng.random_range(0..if depth == 0 { 5 } else { 7 }) {
        0 => Value::Null,
        1 => json!(rng.random::<bool>()),
        2 => json!(rng.random::<i64>()),
        3 => json!(rng.random::<f64>()),
        4 => json!(noise(rng, 12)),
        5 => Value::Array(
            (0..rng.random_range(0..4))
                .map(|_| any_json(rng, depth - 1))
                .collect(),
        ),
        _ => {
            let keys = [
                "name",
                "x",
                "y",
                "label",
                "expression",
                "kind",
                "text",
                "to",
                "bonus",
            ];
            Value::Object(
                (0..rng.random_range(0..5))
                    .map(|_| {
                        let key = keys[rng.random_range(0..keys.len())].to_string();
                        (key, any_json(rng, depth - 1))
                    })
                    .collect(),
            )
        }
    }
}

#[test]
#[ignore]
fn links_and_websites_never_panic() {
    let mut rng = StdRng::seed_from_u64(1);
    for _ in 0..ROUNDS * 10 {
        let text = noise(&mut rng, 40);
        let _ = website::read_link(&text);
        let _ = website::read_link(&format!("yukivtt://pair?website={text}&token={text}"));
        if let Some(origin) = website::origin_of(&text) {
            // Whatever it keeps is https, or plain http close to home.
            assert!(
                origin.starts_with("https://") || origin.starts_with("http://"),
                "{origin}"
            );
        }
    }
}

#[test]
#[ignore]
fn signed_characters_never_panic() {
    let mut rng = StdRng::seed_from_u64(2);
    let good: Value = serde_json::from_str(
        signed("Tam", "u", 18, fresh(), For::Seat("s"))["snapshot"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    for _ in 0..ROUNDS {
        // Broken text, and the real shape with one part swapped for anything.
        let mut shaped = good.clone();
        let keys = [
            "summary",
            "character",
            "issuedAt",
            "seatId",
            "facts",
            "member",
            "play",
        ];
        shaped[keys[rng.random_range(0..keys.len())]] = any_json(&mut rng, 3);
        for text in [noise(&mut rng, 200), shaped.to_string()] {
            let unsigned = Signed {
                snapshot: text.clone(),
                signature: noise(&mut rng, 70),
            };
            let _ = unsigned.open(Some(SECRET));
            let signed: Value = sign(&text);
            let signed = Signed {
                snapshot: text,
                signature: signed["signature"].as_str().unwrap().into(),
            };
            if let Ok(snapshot) = signed.open(Some(SECRET)) {
                let _ = snapshot.state();
                let _ = snapshot.check_save("s", &snapshot, yuki_vtt_server::ids::now());
            }
        }
    }
}

#[test]
#[ignore]
fn pictures_never_panic() {
    let mut rng = StdRng::seed_from_u64(3);
    let png = b"\x89PNG\r\n\x1a\n";
    for _ in 0..ROUNDS * 10 {
        let mut data = bytes(&mut rng, 32);
        if rng.random_bool(0.3) {
            data.splice(0..0, png.iter().copied());
        }
        let _ = pictures::picture_type(&data, rng.random_range(0..64));
    }
}

#[tokio::test]
#[ignore]
async fn backups_never_panic() {
    let mut rng = StdRng::seed_from_u64(4);
    let data = tempfile::tempdir().unwrap();
    let file = data.path().join("odd.yukitable");
    let mut restored = 0;
    for round in 0..2_000 {
        // Raw junk, junk that unzips, and a real archive holding odd rows.
        let contents = match round % 3 {
            0 => bytes(&mut rng, 2048),
            1 => {
                let mut zipped =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                std::io::Write::write_all(&mut zipped, &bytes(&mut rng, 4096)).unwrap();
                zipped.finish().unwrap()
            }
            _ => {
                let manifest = json!({
                    "format": if rng.random_bool(0.8) { json!(1) } else { any_json(&mut rng, 1) },
                    "rows": {
                        "tables": [{
                            "id": if rng.random_bool(0.5) { "Ab1".to_string() } else { noise(&mut rng, 12) },
                            "name": if rng.random_bool(0.7) { json!("Odd table") } else { any_json(&mut rng, 1) },
                            "edition": "2014", "dm_name": "DM", "invite_token": noise(&mut rng, 8),
                            "created_at": 1, "updated_at": 1,
                            "banned": if rng.random_bool(0.7) { json!("[]") } else { any_json(&mut rng, 1) },
                            "encounter": if rng.random_bool(0.7) { Value::Null } else { any_json(&mut rng, 2) },
                        }],
                        "seats": if rng.random_bool(0.5) { json!([]) } else {
                            json!([any_json(&mut rng, 2), { "table_id": "Ab1", "id": any_json(&mut rng, 0), "snapshot": any_json(&mut rng, 1) }])
                        },
                        "log": if rng.random_bool(0.5) { json!([]) } else {
                            json!([{ "table_id": "Ab1", "id": noise(&mut rng, 6), "kind": if rng.random_bool(0.5) { json!("change") } else { any_json(&mut rng, 0) }, "author": "x", "data": any_json(&mut rng, 2), "created_at": 1 }])
                        },
                    },
                })
                .to_string();
                let zipped = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                let mut archive = tar::Builder::new(zipped);
                let mut header = tar::Header::new_gnu();
                header.set_size(manifest.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive
                    .append_data(&mut header, "table.json", manifest.as_bytes())
                    .unwrap();
                let picture = bytes(&mut rng, 64);
                let mut header = tar::Header::new_gnu();
                header.set_size(picture.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                let name = format!("pictures/{}", noise(&mut rng, 10)).replace('\u{0}', "");
                let _ = archive.append_data(&mut header, name, picture.as_slice());
                archive.into_inner().unwrap().finish().unwrap()
            }
        };
        std::fs::write(&file, contents).unwrap();
        // A table that came in from a backup opens without breaking the host.
        if let Ok((id, _)) = backup::restore(data.path(), &file, true) {
            restored += 1;
            let config = yuki_vtt_server::state::Config {
                owner_key_hash: yuki_vtt_server::ids::hash(KEY),
                website_secret: Some(SECRET.to_vec()),
                allowed_origins: vec![SITE.into()],
                data_dir: data.path().to_path_buf(),
            };
            let db = yuki_vtt_server::db::open(&data.path().join("tables.db")).unwrap();
            let app = yuki_vtt_server::app(yuki_vtt_server::state::AppState::new(db, config));
            for path in [
                format!("/tables/{id}"),
                format!("/tables/{id}/party"),
                "/tables".into(),
            ] {
                let (status, answer) = call(&app, As::Dm, "GET", &path, None).await;
                assert_ne!(
                    status,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "{path}: {answer}"
                );
            }
        }
    }
    assert!(restored > 0, "no odd backup got far enough to be restored");
    // Nothing ever lands outside the pictures folder.
    for entry in walk(data.path()) {
        let inside = entry.starts_with(data.path().join("pictures"))
            || entry
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("tables.db"))
            || entry == file;
        assert!(inside, "{}", entry.display());
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[tokio::test]
#[ignore]
async fn routes_never_answer_500() {
    let mut rng = StdRng::seed_from_u64(5);
    let app = host();
    let t = table(&app).await;
    let (seat, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Cave" })),
    )
    .await;
    let scene = format!("/tables/{}/scenes/{}", t.id, made["id"].as_str().unwrap());
    let paths = [
        format!("/tables/{}", t.id),
        format!("/tables/{}/log", t.id),
        format!("/tables/{}/roll", t.id),
        format!("/tables/{}/scenes", t.id),
        format!("/tables/{}/ping", t.id),
        format!("/tables/{}/encounter", t.id),
        format!("/tables/{}/encounter/combatants", t.id),
        format!("/tables/{}/seats/{seat}/change", t.id),
        format!("/tables/{}/seats/{seat}/permissions", t.id),
        format!("/tables/{}/seats/{seat}/role", t.id),
        format!("/tables/{}/handout", t.id),
        format!("/tables/{}/ask", t.id),
        format!("/tables/{}/lock", t.id),
        "/seat".to_string(),
        format!("/join/{}", t.invite),
        "/tables".to_string(),
        format!("/tables/{}/sight", t.id),
        format!("{scene}/lighting"),
        format!("{scene}/explored"),
        format!("{scene}/notes"),
        format!("{scene}/doors/door"),
        format!("{scene}/tokens/party"),
    ];
    let methods = ["GET", "POST", "PUT", "PATCH", "DELETE"];
    for _ in 0..ROUNDS / 4 {
        let path = if rng.random_bool(0.9) {
            paths[rng.random_range(0..paths.len())].clone()
        } else {
            format!(
                "/tables/{}/{}",
                t.id,
                noise(&mut rng, 10).replace(['?', '#', ' ', '%', '\u{0}'], "")
            )
        };
        if !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.~/".contains(c))
        {
            continue;
        }
        let who = match rng.random_range(0..3) {
            0 => As::Dm,
            1 => As::Seat(&token),
            _ => As::Nobody,
        };
        let method = methods[rng.random_range(0..methods.len())];
        // Never delete the table or the seat out from under the rest of the run.
        if method == "DELETE" && (path.ends_with(&t.id) || path == "/seat") {
            continue;
        }
        let body = rng.random_bool(0.9).then(|| any_json(&mut rng, 3));
        let (status, answer) = call(&app, who, method, &path, body.clone()).await;
        assert_ne!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{method} {path} {body:?}: {answer}"
        );
    }
}
