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
    let cave = laid_picture(&app, &t, scene, PNG, (700, 700)).await;
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/roll", t.id),
        Some(json!({ "label": "Luck", "expression": "1d20" })),
    )
    .await;
    // The fog comes along too: walls, shared sight and what the party explored.
    let fog = json!({
        "fog": { "mode": "dynamic", "light": "dark", "remember": true },
        "walls": [{ "id": "w", "a": [0, 70], "b": [700, 70] }],
        "lights": [],
    });
    let at = format!("/tables/{}/scenes/{scene}", t.id);
    call(&app, As::Dm, "PUT", &format!("{at}/lighting"), Some(fog)).await;
    let shared = json!({ "shared": true });
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/sight", t.id),
        Some(shared),
    )
    .await;
    let seen = json!({ "cells": "0:0-3" });
    call(
        &app,
        As::Seat(&token),
        "POST",
        &format!("{at}/explored"),
        Some(seen),
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
    assert_eq!(view["scene"]["walls"][0]["id"], "w");
    assert_eq!(view["party"]["sharedSight"], true);
    let (_, explored) = call(
        &moved,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}/scenes/{scene}/explored", t.id),
        None,
    )
    .await;
    assert_eq!(explored["cells"], "0:0-3");
    let (status, kind, size) = fetch(
        &moved,
        &format!(
            "/tables/{}/assets/{cave}?v=1&pass={}",
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

#[tokio::test]
async fn turns_down_a_backup_whose_rows_the_host_couldnt_read() {
    // A table whose bans aren't a list would break each time it is opened.
    let data = tempfile::tempdir().unwrap();
    let damaged = data.path().join("damaged.yukitable");
    let manifest = serde_json::json!({
        "format": 1,
        "rows": { "tables": [{
            "id": "Ab1", "name": "Odd", "edition": "2014", "dm_name": "DM",
            "invite_token": "inv", "banned": "5", "created_at": 1, "updated_at": 1,
        }] },
    })
    .to_string();
    let zipped = flate2::write::GzEncoder::new(
        std::fs::File::create(&damaged).unwrap(),
        flate2::Compression::fast(),
    );
    let mut archive = tar::Builder::new(zipped);
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "table.json", manifest.as_bytes())
        .unwrap();
    archive.into_inner().unwrap().finish().unwrap();
    assert_eq!(
        backup::restore(data.path(), &damaged, false).unwrap_err(),
        "The backup's tables are damaged"
    );
}

#[tokio::test]
async fn keeps_a_fresh_copy_of_each_table_that_changed() {
    let data = tempfile::tempdir().unwrap();
    let app = host_in(data.path());
    let t = table(&app).await;
    let folder = backup::copies_folder(data.path()).join(&t.id);
    let copies = || {
        std::fs::read_dir(&folder)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "yukitable"))
                    .count()
            })
            .unwrap_or(0)
    };
    assert_eq!(
        backup::copy_changed(data.path()).unwrap(),
        std::slice::from_ref(&t.id)
    );
    assert_eq!(copies(), 1);
    // Nothing changed: no new copy.
    assert!(backup::copy_changed(data.path()).unwrap().is_empty());
    // A change on a map counts, though the table's own time doesn't move.
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Cave" })),
    )
    .await;
    assert!(made["id"].is_string());
    assert_eq!(backup::copy_changed(data.path()).unwrap().len(), 1);
    assert_eq!(copies(), 2);
    // Only the newest ten are kept.
    for i in 0..12 {
        call(
            &app,
            As::Dm,
            "POST",
            &format!("/tables/{}/roll", t.id),
            Some(json!({ "label": format!("Roll {i}"), "expression": "1d20" })),
        )
        .await;
        backup::copy_changed(data.path()).unwrap();
    }
    assert_eq!(copies(), 10);
    // A copy is a table backup like any other.
    let newest = std::fs::read_dir(&folder)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yukitable"))
        .max()
        .unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let (id, _) = backup::restore(elsewhere.path(), &newest, false).unwrap();
    assert_eq!(id, t.id);
}
