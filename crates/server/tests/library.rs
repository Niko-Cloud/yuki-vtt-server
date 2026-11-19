//! One picture library for every table on the host, as the website keeps one per DM.

mod common;

use axum::Router;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use yuki_vtt_server::state::{AppState, Config};
use yuki_vtt_server::{app, backup, db, ids, pictures};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-a-library-picture";

fn host_in(data: &std::path::Path) -> Router {
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

async fn view(app: &Router, who: As<'_>, t: &Table) -> Value {
    call(app, who, "GET", &format!("/tables/{}", t.id), None)
        .await
        .1
}

async fn add(app: &Router, t: &Table, kind: &str) -> Value {
    let path = format!(
        "/tables/{}/assets?kind={kind}&name=Ogre&width=64&height=64",
        t.id
    );
    let (status, added) = upload(app, As::Dm, "POST", &path, PNG).await;
    assert_eq!(status, StatusCode::OK);
    added
}

async fn cave(app: &Router, t: &Table) -> String {
    let (_, made) = call(
        app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Cave" })),
    )
    .await;
    made["id"].as_str().unwrap().to_string()
}

fn ids_of(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn keeps_one_library_for_every_table() {
    let app = host();
    let first = table(&app).await;
    let second = table(&app).await;
    let (seat, token) = join(&app, &second.invite, "Tam", "user-1").await;
    let ogre = add(&app, &first, "token").await;
    assert_eq!(ogre["here"], true);
    let ogre = ogre["id"].as_str().unwrap().to_string();
    let chest = add(&app, &first, "prop").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The other table sees the same picture and may put it on its map.
    let library = view(&app, As::Dm, &second).await["assets"].clone();
    let seen = library.as_array().unwrap().iter().find(|a| a["id"] == ogre);
    assert_eq!(seen.unwrap()["here"], false);
    // A table's first map is the one its players see.
    let scene = cave(&app, &second).await;
    let (status, _) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes/{scene}/tokens", second.id),
        Some(json!({ "name": "Ogre", "x": 1, "y": 1, "imageId": ogre })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Its players get the pictures on their map, and nothing else from the other tables.
    let theirs = view(&app, As::Seat(&token), &second).await["assets"].clone();
    assert_eq!(ids_of(&theirs), std::slice::from_ref(&ogre));

    // A co-DM deletes only what was uploaded at their table.
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/seats/{seat}/role", second.id),
        Some(json!({ "role": "codm" })),
    )
    .await;
    let path = format!("/tables/{}/assets/{ogre}", second.id);
    let (status, _) = call(&app, As::Seat(&token), "DELETE", &path, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A deleted table leaves its pictures in the library.
    call(
        &app,
        As::Dm,
        "DELETE",
        &format!("/tables/{}", first.id),
        None,
    )
    .await;
    let left = ids_of(&view(&app, As::Dm, &second).await["assets"]);
    assert!(left.contains(&ogre) && left.contains(&chest));
    let pass = seat_pass(&app, &second.id, &token).await;
    let (status, _, size) = fetch(&app, &format!("{path}?pass={pass}")).await;
    assert_eq!((status, size), (StatusCode::OK, PNG.len()));

    // The DM deleting it takes it off every table's map.
    let (status, _) = call(&app, As::Dm, "DELETE", &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let tokens = view(&app, As::Dm, &second).await["scene"]["tokens"].clone();
    assert_eq!(tokens[0].get("imageId"), None);
}

#[test]
fn gathers_pictures_kept_beside_their_table() {
    let data = tempfile::tempdir().unwrap();
    let db = db::open(&data.path().join("tables.db")).unwrap();
    db.execute(
        "INSERT INTO assets (id, table_id, kind, name, mime, width, height, created_at)
         VALUES ('pic-1', 'tab-1', 'token', 'Ogre', 'image/png', 64, 64, 0)",
        [],
    )
    .unwrap();
    let old = data.path().join("pictures").join("tab-1");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("pic-1.pic"), PNG).unwrap();

    pictures::gather_library(data.path(), &db).unwrap();
    let moved = pictures::library_folder(data.path()).join("pic-1.pic");
    assert_eq!(std::fs::read(moved).unwrap(), PNG);
    assert!(!old.join("pic-1.pic").exists());
    // Running again changes nothing.
    pictures::gather_library(data.path(), &db).unwrap();
}

#[tokio::test]
async fn backs_up_the_library_pictures_a_table_uses() {
    let first = tempfile::tempdir().unwrap();
    let app = host_in(first.path());
    let elsewhere = table(&app).await;
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let ogre = add(&app, &elsewhere, "token").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let unused = add(&app, &elsewhere, "prop").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let scene = cave(&app, &t).await;
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes/{scene}/tokens", t.id),
        Some(json!({ "name": "Ogre", "x": 1, "y": 1, "imageId": ogre })),
    )
    .await;

    let file = first.path().join("night.yukitable");
    backup::export(first.path(), &t.id, &file).unwrap();
    let second = tempfile::tempdir().unwrap();
    backup::restore(second.path(), &file, false).unwrap();
    let moved = host_in(second.path());

    // The picture its map uses came along; one it never used stayed behind.
    let library = ids_of(&view(&moved, As::Dm, &t).await["assets"]);
    assert_eq!(library, std::slice::from_ref(&ogre));
    assert!(!library.contains(&unused));
    let pass = seat_pass(&moved, &t.id, &token).await;
    let (status, _, size) = fetch(
        &moved,
        &format!("/tables/{}/assets/{ogre}?pass={pass}", t.id),
    )
    .await;
    assert_eq!((status, size), (StatusCode::OK, PNG.len()));

    // Bringing it back over itself keeps the library as it is.
    backup::restore(second.path(), &file, true).unwrap();
    assert_eq!(ids_of(&view(&moved, As::Dm, &t).await["assets"]), [ogre]);
}
