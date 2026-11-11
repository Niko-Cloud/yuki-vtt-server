//! A table backed up and brought back, pictures and all.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use yuki_vtt_server::state::{AppState, Config};
use yuki_vtt_server::{app, backup, db, ids};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-a-map-picture";

fn host_in(data: &std::path::Path) -> axum::Router {
    let config = Config {
        owner_key_hash: ids::hash(KEY),
        website_secret: Some(SECRET.to_vec()),
        allowed_origins: vec![SITE.into()],
        data_dir: data.to_path_buf(),
    };
    app(AppState::new(
        db::open(&data.join("tables.db")).unwrap(),
        config,
    ))
}

#[tokio::test]
async fn backs_up_a_table_and_brings_it_back_on_another_computer() {
    let first = tempfile::tempdir().unwrap();
    let app = host_in(first.path());
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Cave" })),
    )
    .await;
    let scene = made["id"].as_str().unwrap();
    upload(
        &app,
        As::Dm,
        "PUT",
        &format!("/tables/{}/scenes/{scene}/image?width=700&height=700", t.id),
        PNG,
    )
    .await;
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/roll", t.id),
        Some(json!({ "label": "Luck", "expression": "1d20" })),
    )
    .await;

    let file = first.path().join("night.yukitable");
    backup::export(first.path(), &t.id, &file).unwrap();

    let second = tempfile::tempdir().unwrap();
    let (id, name) = backup::restore(second.path(), &file, false).unwrap();
    assert_eq!(
        (id.as_str(), name.as_str()),
        (t.id.as_str(), "Tuesday group")
    );
    let moved = host_in(second.path());
    // The player's seat token works on the new computer, and everything came along.
    let (status, view) = call(
        &moved,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["members"][0]["name"], "Tam");
    assert_eq!(view["scene"]["name"], "Cave");
    assert_eq!(view["log"][0]["data"]["label"], "Luck");
    let (status, kind, size) = fetch(
        &moved,
        &format!(
            "/tables/{}/scenes/{scene}/image?v=1&pass={}",
            t.id,
            seat_pass(&moved, &t.id, &token).await
        ),
    )
    .await;
    assert_eq!(
        (status, kind.as_str(), size),
        (StatusCode::OK, "image/png", PNG.len())
    );

    // Bringing it back again asks first.
    assert!(
        backup::restore(second.path(), &file, false)
            .unwrap_err()
            .contains("already on this computer")
    );
    backup::restore(second.path(), &file, true).unwrap();
}

#[tokio::test]
async fn turns_down_files_that_are_not_backups() {
    let data = tempfile::tempdir().unwrap();
    let junk = data.path().join("junk.yukitable");
    std::fs::write(&junk, b"not a backup").unwrap();
    assert_eq!(
        backup::restore(data.path(), &junk, false).unwrap_err(),
        "That isn't a table backup"
    );
    assert!(backup::export(data.path(), "nope", &data.path().join("x")).is_err());
}

#[tokio::test]
async fn turns_down_a_backup_that_unpacks_into_too_much() {
    // A picture of zeros packs into almost nothing and unpacks past what a map may be.
    let data = tempfile::tempdir().unwrap();
    let bomb = data.path().join("bomb.yukitable");
    let zipped = flate2::write::GzEncoder::new(
        std::fs::File::create(&bomb).unwrap(),
        flate2::Compression::best(),
    );
    let mut archive = tar::Builder::new(zipped);
    let zeros = vec![0u8; 64 * 1024 * 1024];
    let mut header = tar::Header::new_gnu();
    header.set_size(zeros.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "pictures/Ab1.map", zeros.as_slice())
        .unwrap();
    archive.into_inner().unwrap().finish().unwrap();
    assert!(std::fs::metadata(&bomb).unwrap().len() < 1024 * 1024);
    assert_eq!(
        backup::restore(data.path(), &bomb, false).unwrap_err(),
        "That backup unpacks into more than a table can hold"
    );
}
