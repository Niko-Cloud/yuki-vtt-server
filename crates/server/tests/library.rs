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
    let order = json!({ "folder": "Monsters", "tags": ["giant"], "favorite": true });
    let path = format!("/tables/{}/assets/{ogre}", t.id);
    call(&app, As::Dm, "PATCH", &path, Some(order)).await;

    let file = first.path().join("night.yukitable");
    backup::export(first.path(), &t.id, &file).unwrap();
    let second = tempfile::tempdir().unwrap();
    backup::restore(second.path(), &file, false).unwrap();
    let moved = host_in(second.path());

    // The picture its map uses came along; one it never used stayed behind.
    let library = ids_of(&view(&moved, As::Dm, &t).await["assets"]);
    assert_eq!(library, std::slice::from_ref(&ogre));
    assert!(!library.contains(&unused));
    let kept = view(&moved, As::Dm, &t).await["assets"][0].clone();
    assert_eq!(
        (&kept["folder"], &kept["tags"], &kept["favorite"]),
        (&json!("Monsters"), &json!(["giant"]), &json!(true))
    );
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

#[tokio::test]
async fn keeps_the_library_in_order() {
    let app = host();
    let t = table(&app).await;
    let (seat, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let ogre = add(&app, &t, "token").await;
    assert_eq!(
        (&ogre["folder"], &ogre["tags"], &ogre["favorite"]),
        (&Value::Null, &json!([]), &json!(false))
    );
    let path = format!("/tables/{}/assets/{}", t.id, ogre["id"].as_str().unwrap());
    let order = json!({
        "name": "Cave ogre",
        "folder": "Monsters",
        "tags": ["Giant", "giant", "cave"],
        "favorite": true,
    });
    let (status, changed) = call(&app, As::Dm, "PATCH", &path, Some(order)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["name"], "Cave ogre");
    assert_eq!(changed["folder"], "Monsters");
    assert_eq!(changed["tags"], json!(["giant", "cave"]));
    assert_eq!(
        (&changed["favorite"], &changed["here"]),
        (&json!(true), &json!(true))
    );

    // Null takes it out of its folder; the rest stays.
    let out = json!({ "folder": null });
    call(&app, As::Dm, "PATCH", &path, Some(out)).await;
    let kept = view(&app, As::Dm, &t).await["assets"][0].clone();
    assert_eq!(kept["folder"], Value::Null);
    assert_eq!(kept["tags"], json!(["giant", "cave"]));

    let many: Vec<String> = (0..11).map(|i| format!("t{i}")).collect();
    let (status, _) = call(&app, As::Dm, "PATCH", &path, Some(json!({ "tags": many }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, As::Dm, "PATCH", &path, Some(json!({ "owner": "me" }))).await;
    assert!(status.is_client_error());

    // Players don't keep the DM's library; a co-DM keeps what was uploaded at their table.
    let star = json!({ "favorite": false });
    let (status, _) = call(&app, As::Seat(&token), "PATCH", &path, Some(star.clone())).await;
    assert!(status.is_client_error());
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/seats/{seat}/role", t.id),
        Some(json!({ "role": "codm" })),
    )
    .await;
    let (status, _) = call(&app, As::Seat(&token), "PATCH", &path, Some(star)).await;
    assert_eq!(status, StatusCode::OK);
}

async fn picture_site() -> u16 {
    use axum::response::{IntoResponse, Redirect};
    use axum::routing::get;
    let site = Router::new()
        .route("/ogre.png", get(|| async { PNG }))
        .route("/moved", get(|| async { Redirect::temporary("/ogre.png") }))
        .route(
            "/inside",
            get(|| async { Redirect::temporary("http://10.0.0.5/x") }),
        )
        .route("/loop", get(|| async { Redirect::temporary("/loop") }))
        .route("/page", get(|| async { "<html>not a picture</html>" }))
        .route(
            "/huge",
            get(|| async { vec![1u8; 13 * 1024 * 1024].into_response() }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, site).await.unwrap() });
    port
}

#[tokio::test]
async fn fetches_pictures_from_the_open_internet_only() {
    use yuki_vtt_server::web_picture::{Rules, fetch};
    let port = picture_site().await;
    // The test's own site is on this computer, which the real rules turn away.
    let here = Rules {
        allowed: |ip| ip.is_loopback(),
        ports: vec![None, Some(port)],
    };
    let at = |path: &str| format!("http://127.0.0.1:{port}{path}");
    let message = |result: Result<(Vec<u8>, &str), yuki_vtt_server::error::AppError>| {
        format!("{:?}", result.expect_err("a refusal"))
    };

    let (data, mime) = fetch(&at("/ogre.png"), &here).await.unwrap();
    assert_eq!((data.as_slice(), mime), (PNG, "image/png"));
    assert_eq!(fetch(&at("/moved"), &here).await.unwrap().1, "image/png");
    assert!(message(fetch(&at("/inside"), &here).await).contains("open internet"));
    assert!(message(fetch(&at("/loop"), &here).await).contains("too many times"));
    assert!(message(fetch(&at("/page"), &here).await).contains("PNG"));
    assert!(message(fetch(&at("/huge"), &here).await).contains("over 12 MB"));
    assert!(message(fetch(&at("/gone"), &here).await).contains("answered 404"));

    let real = Rules::default();
    let local = fetch("http://127.0.0.1/ogre.png", &real).await;
    assert!(message(local).contains("open internet"));
    assert!(message(fetch("http://localhost/ogre.png", &real).await).contains("open internet"));
    assert!(message(fetch("ftp://example.com/a.png", &real).await).contains("http or https"));
    assert!(message(fetch("http://me:pw@example.com/a.png", &real).await).contains("password"));
    assert!(message(fetch("http://example.com:6379/", &real).await).contains("usual port"));
}

#[tokio::test]
async fn fetches_for_whoever_may_add_pictures() {
    let app = host();
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let path = format!("/tables/{}/web-picture", t.id);
    let inside = json!({ "url": "http://169.254.169.254/latest/meta-data" });
    let (status, _) = call(&app, As::Seat(&token), "POST", &path, Some(inside.clone())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, refused) = call(&app, As::Dm, "POST", &path, Some(inside)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(refused.to_string().contains("open internet"));
}
