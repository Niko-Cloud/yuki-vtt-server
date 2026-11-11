//! Maps, tokens, pictures and drawings, as the map room uses them.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

// The smallest PNG there is, as far as the host can tell from its first bytes.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-not-really-but-the-header-is-right";

async fn scene(app: &axum::Router, t: &Table, name: &str) -> String {
    let (status, made) = call(
        app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    made["id"].as_str().unwrap().to_string()
}

async fn view(app: &axum::Router, who: As<'_>, t: &Table) -> Value {
    call(app, who, "GET", &format!("/tables/{}", t.id), None)
        .await
        .1
}

#[tokio::test]
async fn shows_players_only_the_map_the_dm_shows() {
    let app = host();
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let cave = scene(&app, &t, "Cave").await;
    let town = scene(&app, &t, "Town").await;

    let seen = view(&app, As::Seat(&token), &t).await;
    assert_eq!(seen["activeSceneId"], cave.as_str());
    assert_eq!(seen["scene"]["name"], "Cave");
    assert_eq!(seen["scene"]["grid"]["size"], 70);
    assert_eq!(seen["scenes"], json!([]));
    // The DM may look at another map; players can't.
    let (_, dm) = call(
        &app,
        As::Dm,
        "GET",
        &format!("/tables/{}?scene={town}", t.id),
        None,
    )
    .await;
    assert_eq!(dm["scene"]["name"], "Town");
    assert_eq!(dm["scenes"].as_array().unwrap().len(), 2);
    let (_, peek) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}?scene={town}", t.id),
        None,
    )
    .await;
    assert_eq!(peek["scene"]["name"], "Cave");

    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes/{town}/show", t.id),
        None,
    )
    .await;
    assert_eq!(
        view(&app, As::Seat(&token), &t).await["scene"]["name"],
        "Town"
    );
    // Deleting the shown map shows the next one.
    call(
        &app,
        As::Dm,
        "DELETE",
        &format!("/tables/{}/scenes/{town}", t.id),
        None,
    )
    .await;
    assert_eq!(
        view(&app, As::Seat(&token), &t).await["activeSceneId"],
        cave.as_str()
    );
}

#[tokio::test]
async fn moves_tokens_as_far_as_the_dm_lets_players() {
    let app = host();
    let t = table(&app).await;
    let (seat, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let cave = scene(&app, &t, "Cave").await;
    let base = format!("/tables/{}/scenes/{cave}/tokens", t.id);
    call(&app, As::Dm, "POST", &format!("{base}/party"), None).await;
    call(
        &app,
        As::Dm,
        "POST",
        &base,
        Some(json!({ "name": "Ogre", "x": 5, "y": 5, "size": 2 })),
    )
    .await;
    call(
        &app,
        As::Dm,
        "POST",
        &base,
        Some(json!({ "name": "Assassin", "x": 8, "y": 2 })),
    )
    .await;

    let dm = view(&app, As::Dm, &t).await;
    let id_of = |name: &str| {
        dm["scene"]["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (tam, ogre, assassin) = (id_of("Tam"), id_of("Ogre"), id_of("Assassin"));
    assert_eq!(dm["scene"]["tokens"][0]["seatId"], seat.as_str());
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("{base}/{assassin}"),
        Some(json!({ "hidden": true })),
    )
    .await;

    // A hidden token doesn't exist for players.
    let names: Vec<String> = view(&app, As::Seat(&token), &t).await["scene"]["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["Tam", "Ogre"]);
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "PATCH",
        &format!("{base}/{assassin}"),
        Some(json!({ "x": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Your own token, yes; the ogre only with "move-any".
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "PATCH",
        &format!("{base}/{tam}"),
        Some(json!({ "x": 3, "y": 4 })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "PATCH",
        &format!("{base}/{ogre}"),
        Some(json!({ "x": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/seats/{seat}/permissions", t.id),
        Some(json!({ "permissions": ["move-own", "move-any"] })),
    )
    .await;
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "PATCH",
        &format!("{base}/{ogre}"),
        Some(json!({ "x": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Renaming still takes "tokens"; locking is the DM's.
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "PATCH",
        &format!("{base}/{ogre}"),
        Some(json!({ "name": "Bob" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("{base}/{ogre}"),
        Some(json!({ "locked": true })),
    )
    .await;
    let (status, refused) = call(
        &app,
        As::Seat(&token),
        "PATCH",
        &format!("{base}/{ogre}"),
        Some(json!({ "x": 2 })),
    )
    .await;
    assert_eq!(
        (status, refused["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("Ogre is locked"))
    );

    // A smaller map keeps the Large ogre whole on it.
    call(&app, As::Dm, "PATCH", &format!("/tables/{}/scenes/{cave}", t.id), Some(json!({ "setup": { "width": 280, "height": 280, "grid": { "size": 70, "offsetX": 0, "offsetY": 0, "feetPerCell": 5, "show": true } } }))).await;
    let ogre_now = view(&app, As::Dm, &t).await["scene"]["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Ogre")
        .unwrap()
        .clone();
    assert_eq!(
        (ogre_now["x"].as_u64(), ogre_now["y"].as_u64()),
        (Some(1), Some(2))
    );
}

#[tokio::test]
async fn keeps_players_off_the_dms_other_maps() {
    let app = host();
    let t = table(&app).await;
    let (seat, token) = join(&app, &t.invite, "Tam", "user-1").await;
    scene(&app, &t, "Cave").await;
    let town = scene(&app, &t, "Town").await;
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/seats/{seat}/permissions", t.id),
        Some(json!({ "permissions": ["tokens", "draw"] })),
    )
    .await;
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "POST",
        &format!("/tables/{}/scenes/{town}/tokens", t.id),
        Some(json!({ "name": "Spy", "x": 0, "y": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn draws_and_erases_by_permission() {
    let app = host();
    let t = table(&app).await;
    let (seat, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let cave = scene(&app, &t, "Cave").await;
    let path = format!("/tables/{}/scenes/{cave}/drawings", t.id);
    let line = json!({ "shape": "line", "color": "#FF9090", "points": [[0, 0], [140, 70]] });
    let (status, _) = call(&app, As::Seat(&token), "POST", &path, Some(line.clone())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/seats/{seat}/permissions", t.id),
        Some(json!({ "permissions": ["draw"] })),
    )
    .await;
    call(&app, As::Seat(&token), "POST", &path, Some(line.clone())).await;
    call(&app, As::Dm, "POST", &path, Some(line)).await;
    let drawings = view(&app, As::Dm, &t).await["scene"]["drawings"].clone();
    assert_eq!(drawings[0]["by"], seat.as_str());
    assert_eq!(drawings[0]["color"], "#ff9090");
    assert_eq!(drawings[1]["by"], "dm");

    // A player erases only their own line.
    let dms = drawings[1]["id"].as_str().unwrap();
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "DELETE",
        &format!("{path}/{dms}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let mine = drawings[0]["id"].as_str().unwrap();
    call(
        &app,
        As::Seat(&token),
        "DELETE",
        &format!("{path}/{mine}"),
        None,
    )
    .await;
    assert_eq!(
        view(&app, As::Dm, &t).await["scene"]["drawings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    call(&app, As::Dm, "DELETE", &path, None).await;
    assert_eq!(view(&app, As::Dm, &t).await["scene"]["drawings"], json!([]));

    let (status, _) = call(
        &app,
        As::Dm,
        "POST",
        &path,
        Some(json!({ "color": "#ffffff", "points": [[0, 0]] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn keeps_map_pictures_for_those_at_the_table() {
    let app = host();
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let cave = scene(&app, &t, "Cave").await;
    let path = format!("/tables/{}/scenes/{cave}/image", t.id);

    let (status, _) = upload(
        &app,
        As::Seat(&token),
        "PUT",
        &format!("{path}?width=1000&height=700"),
        PNG,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, refused) = upload(
        &app,
        As::Dm,
        "PUT",
        &format!("{path}?width=1000&height=700"),
        b"<svg></svg>",
    )
    .await;
    assert_eq!(
        (status, refused["message"].as_str()),
        (
            StatusCode::BAD_REQUEST,
            Some("Use a PNG, JPEG or WebP picture")
        )
    );
    let (status, _) = upload(
        &app,
        As::Dm,
        "PUT",
        &format!("{path}?width=1000&height=700"),
        PNG,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let map = view(&app, As::Seat(&token), &t).await["scene"].clone();
    assert_eq!(
        (map["width"].as_u64(), map["image"]["mime"].as_str()),
        (Some(1000), Some("image/png"))
    );
    let pass = seat_pass(&app, &t.id, &token).await;
    let (status, kind, size) = fetch(&app, &format!("{path}?v=1&pass={pass}")).await;
    assert_eq!(
        (status, kind.as_str(), size),
        (StatusCode::OK, "image/png", PNG.len())
    );
    // The seat token itself no longer goes in an address.
    let (status, _, _) = fetch(&app, &format!("{path}?v=1&seat={token}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = fetch(&app, &format!("{path}?v=1&pass=wrong")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn keeps_a_picture_library_and_shows_handouts() {
    let app = host();
    let t = table(&app).await;
    let (seat, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let cave = scene(&app, &t, "Cave").await;
    let add = |kind: &str| {
        format!(
            "/tables/{}/assets?kind={kind}&name=Map%20note&width=300&height=200",
            t.id
        )
    };

    // A player adds a portrait only when the DM lets them change tokens, and never a handout.
    let (status, _) = upload(&app, As::Seat(&token), "POST", &add("token"), PNG).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/seats/{seat}/permissions", t.id),
        Some(json!({ "permissions": ["tokens"] })),
    )
    .await;
    let (status, portrait) = upload(&app, As::Seat(&token), "POST", &add("token"), PNG).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = upload(&app, As::Seat(&token), "POST", &add("handout"), PNG).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, letter) = upload(&app, As::Dm, "POST", &add("handout"), PNG).await;
    assert_eq!(letter["name"], "Map note");

    let portrait = portrait["id"].as_str().unwrap();
    let letter = letter["id"].as_str().unwrap();
    call(
        &app,
        As::Seat(&token),
        "POST",
        &format!("/tables/{}/scenes/{cave}/tokens", t.id),
        Some(json!({ "name": "Owl", "x": 1, "y": 1, "imageId": portrait })),
    )
    .await;
    // Null takes the picture off again.
    let owl = view(&app, As::Dm, &t).await["scene"]["tokens"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let owl_at = format!("/tables/{}/scenes/{cave}/tokens/{owl}", t.id);
    let off = json!({ "imageId": null });
    call(&app, As::Seat(&token), "PATCH", &owl_at, Some(off)).await;
    let tokens = view(&app, As::Dm, &t).await["scene"]["tokens"].clone();
    assert_eq!(tokens[0].get("imageId"), None);
    let back = json!({ "imageId": portrait });
    call(&app, As::Seat(&token), "PATCH", &owl_at, Some(back)).await;
    // Handouts reach players only when shown.
    let kinds: Vec<Value> = view(&app, As::Seat(&token), &t).await["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].clone())
        .collect();
    assert_eq!(kinds, [json!("token")]);
    call(
        &app,
        As::Dm,
        "PUT",
        &format!("/tables/{}/handout", t.id),
        Some(json!({ "assetId": letter, "title": "A letter" })),
    )
    .await;
    assert_eq!(
        view(&app, As::Seat(&token), &t).await["handout"],
        json!({ "assetId": letter, "title": "A letter" })
    );
    let (status, kind, _) = fetch(
        &app,
        &format!(
            "/tables/{}/assets/{letter}?v=1&pass={}",
            t.id,
            seat_pass(&app, &t.id, &token).await
        ),
    )
    .await;
    assert_eq!((status, kind.as_str()), (StatusCode::OK, "image/png"));

    // Deleting a picture takes it off the map and stops the handout.
    call(
        &app,
        As::Dm,
        "DELETE",
        &format!("/tables/{}/assets/{portrait}", t.id),
        None,
    )
    .await;
    call(
        &app,
        As::Dm,
        "DELETE",
        &format!("/tables/{}/assets/{letter}", t.id),
        None,
    )
    .await;
    let after = view(&app, As::Dm, &t).await;
    assert_eq!(after["scene"]["tokens"][0].get("imageId"), None);
    assert_eq!(after["handout"], Value::Null);
    assert_eq!(after["assets"], json!([]));
}

#[tokio::test]
async fn pings_need_a_map_and_the_right_to_draw() {
    let app = host();
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let (status, _) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/ping", t.id),
        Some(json!({ "x": 10, "y": 10 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    scene(&app, &t, "Cave").await;
    let (status, _) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/ping", t.id),
        Some(json!({ "x": 10, "y": 10 })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "POST",
        &format!("/tables/{}/ping", t.id),
        Some(json!({ "x": 10, "y": 10 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sends_a_map_saved_before_fog_with_fog_off() {
    let data = tempfile::tempdir().unwrap();
    let config = yuki_vtt_server::state::Config {
        owner_key_hash: yuki_vtt_server::ids::hash(KEY),
        website_secret: Some(SECRET.to_vec()),
        allowed_origins: vec![SITE.into()],
        data_dir: data.path().to_path_buf(),
    };
    let db = yuki_vtt_server::db::open(&data.path().join("tables.db")).unwrap();
    let app = yuki_vtt_server::app(yuki_vtt_server::state::AppState::new(db, config));
    let t = table(&app).await;
    let cave = scene(&app, &t, "Cave").await;
    // The map as an older host saved it: no fog, walls, lights or notes.
    let old = json!({
        "width": 700, "height": 490, "image": null, "tokens": [], "props": [], "drawings": [],
        "grid": { "size": 70, "offsetX": 0, "offsetY": 0, "feetPerCell": 5, "show": true },
    });
    yuki_vtt_server::db::open(&data.path().join("tables.db"))
        .unwrap()
        .execute(
            "UPDATE scenes SET data = ?1 WHERE id = ?2",
            [old.to_string(), cave],
        )
        .unwrap();
    let seen = view(&app, As::Dm, &t).await;
    assert_eq!(seen["scene"]["fog"]["mode"], "off");
    assert_eq!(seen["scene"]["walls"], json!([]));
    assert_eq!(seen["scene"]["notes"], json!([]));
}
