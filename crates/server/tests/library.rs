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
    assert_eq!(
        (&ogre["here"], &ogre["size"]),
        (&json!(true), &json!(PNG.len()))
    );
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
fn gathers_pictures_kept_the_old_way_into_readable_folders() {
    let data = tempfile::tempdir().unwrap();
    let db = db::open(&data.path().join("tables.db")).unwrap();
    db.execute_batch(
        "INSERT INTO tables (id, name, edition, dm_name, invite_token, created_at, updated_at)
           VALUES ('tab-1', 'Tuesday', '2014', 'Yuki', 'inv-1', 0, 0);
         INSERT INTO assets (id, table_id, kind, name, mime, width, height, created_at)
           VALUES ('pic-1', 'tab-1', 'token', 'Ogre Chief', 'image/png', 64, 64, 0),
                  ('pic-2', 'tab-1', 'prop', 'Chest', 'image/png', 64, 64, 0);
         INSERT INTO scenes (id, table_id, name, position, data, created_at)
           VALUES ('scn-1', 'tab-1', 'Goblin cave', 0,
                   '{\"width\":700,\"height\":700,\"image\":{\"mime\":\"image/png\",\"version\":1}}', 0);",
    )
    .unwrap();
    // One beside its table (the oldest hosts), one in the id-named library, and a map.
    let beside = data.path().join("pictures").join("tab-1");
    std::fs::create_dir_all(&beside).unwrap();
    std::fs::write(beside.join("pic-1.pic"), PNG).unwrap();
    std::fs::write(beside.join("scn-1.map"), PNG).unwrap();
    std::fs::create_dir_all(data.path().join("library")).unwrap();
    std::fs::write(data.path().join("library").join("pic-2.pic"), PNG).unwrap();

    pictures::gather_library(data.path(), &db).unwrap();
    let library = data.path().join("Library");
    for path in [
        "Tokens/Ogre Chief/ogre-chief.png",
        "Props/Chest/chest.png",
        "Maps/Goblin cave/map.png",
    ] {
        assert_eq!(std::fs::read(library.join(path)).unwrap(), PNG, "{path}");
    }
    let (file, size): (String, i64) = db
        .query_row(
            "SELECT file, size FROM assets WHERE id = 'pic-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (file.as_str(), size),
        ("Tokens/Ogre Chief/ogre-chief.png", PNG.len() as i64)
    );
    // The old folders are gone, and running again changes nothing.
    assert!(!data.path().join("pictures").exists());
    assert!(!data.path().join("library").exists());
    pictures::gather_library(data.path(), &db).unwrap();
    assert!(library.join("Tokens/Ogre Chief/ogre-chief.png").is_file());
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
    let filed = json!({ "folder": "Act one" });
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/scenes/{scene}", t.id),
        Some(filed),
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
    // The map keeps its folder too.
    assert_eq!(
        view(&moved, As::Dm, &t).await["scenes"][0]["folder"],
        "Act one"
    );
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

#[tokio::test]
async fn keeps_each_picture_in_its_own_readable_folder() {
    let data = tempfile::tempdir().unwrap();
    let app = host_in(data.path());
    let t = table(&app).await;
    let library = data.path().join("Library");
    let upload_as = |name: &'static str| {
        let app = app.clone();
        let id = t.id.clone();
        async move {
            let path = format!("/tables/{id}/assets?kind=token&name={name}&width=64&height=64");
            upload(&app, As::Dm, "POST", &path, PNG).await.1["id"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let ogre = upload_as("Ogre%20Chief").await;
    assert!(library.join("Tokens/Ogre Chief/ogre-chief.png").is_file());
    // The same name gets its own folder.
    upload_as("Ogre%20Chief").await;
    assert!(
        library
            .join("Tokens/Ogre Chief (2)/ogre-chief.png")
            .is_file()
    );

    // Renaming in the app renames the folder, and anything kept beside the picture moves along.
    std::fs::write(library.join("Tokens/Ogre Chief/notes.txt"), "keep").unwrap();
    let rename = json!({ "name": "Ogre King" });
    let at = format!("/tables/{}/assets/{ogre}", t.id);
    call(&app, As::Dm, "PATCH", &at, Some(rename)).await;
    assert!(library.join("Tokens/Ogre King/ogre-king.png").is_file());
    assert!(!library.join("Tokens/Ogre Chief").exists());
    let pass = pass_for(&app, &t.id).await;
    let (status, _, size) = fetch(&app, &format!("{at}?pass={pass}")).await;
    assert_eq!((status, size), (StatusCode::OK, PNG.len()));

    // Deleting takes the file away, and its folder once empty.
    std::fs::remove_file(library.join("Tokens/Ogre King/notes.txt")).unwrap();
    call(&app, As::Dm, "DELETE", &at, None).await;
    assert!(!library.join("Tokens/Ogre King").exists());

    // A map picture lives under Maps; a board copied or deleted leaves it in the library.
    let scene = cave(&app, &t).await;
    laid_picture(&app, &t, &scene, PNG, (700, 700)).await;
    let kept = || std::fs::read_dir(library.join("Maps/Cave")).map_or(0, |files| files.count());
    assert_eq!(kept(), 1);
    let map_at = format!("/tables/{}/scenes/{scene}", t.id);
    let (_, copy) = call(&app, As::Dm, "POST", &format!("{map_at}/copy"), None).await;
    let copy_at = format!("/tables/{}/scenes/{}", t.id, copy["id"].as_str().unwrap());
    call(&app, As::Dm, "DELETE", &copy_at, None).await;
    call(&app, As::Dm, "DELETE", &map_at, None).await;
    assert_eq!(kept(), 1);
}

async fn pass_for(app: &Router, table: &str) -> String {
    let (_, given) = call(
        app,
        As::Dm,
        "POST",
        &format!("/tables/{table}/media-token"),
        None,
    )
    .await;
    given["pass"].as_str().unwrap().to_string()
}

// A real (1 by 1) PNG: the scan reads a picture's size from it.
const REAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

fn state_in(data: &std::path::Path) -> AppState {
    let config = Config {
        owner_key_hash: ids::hash(KEY),
        website_secret: Some(SECRET.to_vec()),
        allowed_origins: vec![SITE.into()],
        data_dir: data.to_path_buf(),
    };
    AppState::new(db::open(&data.join("tables.db")).unwrap(), config)
}

fn named(library: &Value) -> Vec<(String, String)> {
    library
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap().to_string(),
                a["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn follows_what_the_dm_changes_in_the_folder_by_hand() {
    use yuki_vtt_server::library;
    let data = tempfile::tempdir().unwrap();
    let state = state_in(data.path());
    let app = app(state.clone());
    let t = table(&app).await;
    let tokens = data.path().join("Library/Tokens");
    std::fs::create_dir_all(tokens.join("Wolf")).unwrap();
    std::fs::write(tokens.join("Wolf/wolf.png"), REAL_PNG).unwrap();
    std::fs::write(tokens.join("bat.png"), REAL_PNG).unwrap();
    std::fs::create_dir_all(tokens.join("Broken")).unwrap();
    std::fs::write(tokens.join("Broken/broken.png"), b"not a picture").unwrap();
    std::fs::write(tokens.join("Wolf/notes.txt"), "bites").unwrap();

    let report = library::rescan(&state);
    assert_eq!(report.added.len(), 2);
    assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
    assert!(report.skipped[0].starts_with("Tokens/Broken/broken.png"));
    let shown = view(&app, As::Dm, &t).await;
    let mut found = named(&shown["assets"]);
    found.sort();
    assert_eq!(
        found,
        [
            ("Wolf".into(), "token".into()),
            ("bat".into(), "token".into())
        ]
    );
    let wolf = shown["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "Wolf")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(shown["assets"][0]["width"], 1);

    // Put on a map, then renamed by hand: the same picture, so the map keeps it.
    let scene = cave(&app, &t).await;
    let token = json!({ "name": "Wolf", "x": 1, "y": 1, "imageId": wolf });
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes/{scene}/tokens", t.id),
        Some(token),
    )
    .await;
    std::fs::rename(tokens.join("Wolf"), tokens.join("Dire Wolf")).unwrap();
    let report = library::rescan(&state);
    assert_eq!(report.renamed, ["Dire Wolf"]);
    let shown = view(&app, As::Dm, &t).await;
    let kept = shown["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == wolf.as_str());
    assert_eq!(kept.unwrap()["name"], "Dire Wolf");
    assert_eq!(shown["scene"]["tokens"][0]["imageId"], wolf.as_str());

    // Deleted by hand: gone from the library, and the token shows its initials again.
    std::fs::remove_dir_all(tokens.join("Dire Wolf")).unwrap();
    let report = library::rescan(&state);
    assert_eq!(report.removed, [wolf]);
    let shown = view(&app, As::Dm, &t).await;
    assert_eq!(named(&shown["assets"]), [("bat".into(), "token".into())]);
    assert_eq!(shown["scene"]["tokens"][0].get("imageId"), None);

    // Scanning again with nothing new changes nothing.
    assert!(!library::rescan(&state).changed());
}

#[tokio::test]
async fn notices_a_picture_copied_in_while_the_host_runs() {
    use yuki_vtt_server::library;
    let data = tempfile::tempdir().unwrap();
    let state = state_in(data.path());
    let app = app(state.clone());
    let t = table(&app).await;
    let watcher = library::watch(state.clone());
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let props = data.path().join("Library/Props/Cart");
    std::fs::create_dir_all(&props).unwrap();
    std::fs::write(props.join("cart.png"), REAL_PNG).unwrap();
    let mut seen = vec![];
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        seen = named(&view(&app, As::Dm, &t).await["assets"]);
        if !seen.is_empty() {
            break;
        }
    }
    watcher.abort();
    assert_eq!(seen, [("Cart".into(), "prop".into())]);
}

#[tokio::test]
async fn opens_the_library_folder_on_the_hosting_computer_for_the_dm() {
    let data = tempfile::tempdir().unwrap();
    let mut state = state_in(data.path());
    let asked: std::sync::Arc<std::sync::Mutex<Vec<std::path::PathBuf>>> = Default::default();
    let noted = asked.clone();
    state.show_on_screen = std::sync::Arc::new(move |path: &std::path::Path| {
        noted.lock().unwrap().push(path.to_path_buf());
        Ok(())
    });
    let app = app(state);
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let ogre = add(&app, &t, "token").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = format!("/tables/{}/library/open", t.id);

    let (status, _) = call(&app, As::Dm, "POST", &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &app,
        As::Dm,
        "POST",
        &path,
        Some(json!({ "assetId": ogre })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, As::Seat(&token), "POST", &path, None).await;
    assert!(status.is_client_error());
    let (status, _) = call(
        &app,
        As::Dm,
        "POST",
        &path,
        Some(json!({ "assetId": "gone" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0], data.path().join("Library"));
    assert_eq!(asked[1], data.path().join("Library/Tokens/Ogre/ogre.png"));
}

#[tokio::test]
async fn sends_players_a_small_clean_copy_of_a_picture_copied_in_by_hand() {
    use yuki_vtt_server::library;
    let data = tempfile::tempdir().unwrap();
    let state = state_in(data.path());
    let app = app(state.clone());
    let t = table(&app).await;
    // A big photo-like PNG with a note in it, as a phone or an editor leaves.
    let picture = image::RgbaImage::from_fn(2400, 1600, |x, y| {
        image::Rgba([(x / 10) as u8, (y / 7) as u8, 128, 255])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(picture)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let text = b"tEXtLocation\x0051.5N 0.12W";
    let mut chunk = (text.len() as u32 - 4).to_be_bytes().to_vec();
    chunk.extend_from_slice(text);
    chunk.extend_from_slice(&[0, 0, 0, 0]);
    png.splice(33..33, chunk);
    let folder = data.path().join("Library/Tokens/Wolf");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("wolf.png"), &png).unwrap();
    library::rescan(&state);
    let wolf = view(&app, As::Dm, &t).await["assets"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, given) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/media-token", t.id),
        None,
    )
    .await;
    let pass = given["pass"].as_str().unwrap();
    let (status, kind, size) =
        fetch(&app, &format!("/tables/{}/assets/{wolf}?pass={pass}", t.id)).await;
    assert_eq!((status, kind.as_str()), (StatusCode::OK, "image/webp"));
    assert!(size * 5 < png.len(), "{size} against {}", png.len());
    assert_eq!(
        std::fs::read(folder.join("wolf.png")).unwrap(),
        png,
        "the DM's file stays"
    );
}

#[tokio::test]
async fn sends_a_small_first_look_at_a_map() {
    let data = tempfile::tempdir().unwrap();
    let app = host_in(data.path());
    let t = table(&app).await;
    let picture = image::RgbaImage::from_fn(1400, 1000, |x, y| {
        image::Rgba([(x / 6) as u8, (y / 4) as u8, 90, 255])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(picture)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let scene = cave(&app, &t).await;
    let picture = laid_picture(&app, &t, &scene, &png, (1400, 1000)).await;
    let at = format!("/tables/{}/assets/{picture}", t.id);
    let (_, given) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/media-token", t.id),
        None,
    )
    .await;
    let pass = given["pass"].as_str().unwrap();

    let (status, kind, small) = fetch(&app, &format!("{at}?v=1&preview=1&pass={pass}")).await;
    assert_eq!((status, kind.as_str()), (StatusCode::OK, "image/webp"));
    let (_, _, whole) = fetch(&app, &format!("{at}?v=1&pass={pass}")).await;
    assert!(small * 4 < whole, "{small} against {whole}");
}

#[tokio::test]
async fn tells_every_screen_to_fetch_a_maps_picture_before_it_is_shown() {
    let data = tempfile::tempdir().unwrap();
    let state = state_in(data.path());
    let app = app(state.clone());
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let scene = cave(&app, &t).await;
    let picture = laid_picture(&app, &t, &scene, PNG, (700, 700)).await;
    let mut heard = state.events.listen(&t.id);
    let at = format!("/tables/{}/scenes/{scene}/preload", t.id);
    let (status, _) = call(&app, As::Seat(&token), "POST", &at, None).await;
    assert!(status.is_client_error());
    let (status, _) = call(&app, As::Dm, "POST", &at, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let message: Value = serde_json::from_str(&heard.recv().await.unwrap()).unwrap();
    assert_eq!(message["type"], "preload");
    assert_eq!(message["preload"]["sceneId"], scene.as_str());
    assert_eq!(
        message["preload"]["pictures"],
        json!([{ "id": picture, "mime": "image/png" }])
    );
}

#[tokio::test]
async fn takes_map_pictures_copied_in_by_hand_once() {
    use yuki_vtt_server::library;
    let data = tempfile::tempdir().unwrap();
    let state = state_in(data.path());
    let app = app(state.clone());
    let t = table(&app).await;
    // A map picture uploaded and laid on a board, kept in Library/Maps.
    let scene = cave(&app, &t).await;
    laid_picture(&app, &t, &scene, REAL_PNG, (100, 100)).await;
    // And one copied in by hand.
    let maps = data.path().join("Library/Maps");
    std::fs::create_dir_all(maps.join("Crypt")).unwrap();
    std::fs::write(maps.join("Crypt/crypt.png"), REAL_PNG).unwrap();

    let report = library::rescan(&state);
    assert_eq!(report.added, ["Crypt"]);
    let mut found = named(&view(&app, As::Dm, &t).await["assets"]);
    found.sort();
    assert_eq!(
        found,
        [
            ("Cave".into(), "map".into()),
            ("Crypt".into(), "map".into())
        ]
    );
    // Scanned again: nothing new.
    assert!(library::rescan(&state).added.is_empty());
}
