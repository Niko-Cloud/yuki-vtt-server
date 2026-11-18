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

    // A player changes only their own line, never its layer; the DM changes any.
    let change = |who, n: usize, body: Value| {
        let path = format!("{path}/{}", drawings[n]["id"].as_str().unwrap());
        let app = app.clone();
        async move { call(&app, who, "PATCH", &path, Some(body)).await.0 }
    };
    let seat_said = change(As::Seat(&token), 1, json!({ "color": "#000000" })).await;
    assert_eq!(seat_said, StatusCode::NOT_FOUND);
    let hide = change(As::Seat(&token), 0, json!({ "hidden": true })).await;
    assert_eq!(hide, StatusCode::NOT_FOUND);
    let recolour = json!({ "color": "#123456", "weight": "thick" });
    assert_eq!(
        change(As::Seat(&token), 0, recolour).await,
        StatusCode::NO_CONTENT
    );
    let odd = change(As::Dm, 0, json!({ "weight": "huge" })).await;
    assert_eq!(odd, StatusCode::BAD_REQUEST);
    let three = json!({ "points": [[0, 0], [1, 1], [2, 2]] });
    assert_eq!(change(As::Dm, 0, three).await, StatusCode::BAD_REQUEST);
    let moved = json!({ "points": [[70, 0], [210, 70]], "hidden": true });
    assert_eq!(change(As::Dm, 0, moved).await, StatusCode::NO_CONTENT);
    let now = view(&app, As::Dm, &t).await["scene"]["drawings"][0].clone();
    assert_eq!(now["color"], "#123456");
    assert_eq!(now["weight"], "thick");
    assert_eq!(now["hidden"], true);
    assert_eq!(now["points"][0], json!([70.0, 0.0]));
    let seen = view(&app, As::Seat(&token), &t).await["scene"]["drawings"].clone();
    assert_eq!(seen.as_array().unwrap().len(), 1);
    assert_eq!(
        change(As::Dm, 0, json!({ "hidden": false })).await,
        StatusCode::NO_CONTENT
    );
    // A filled shape keeps its whole outline.
    let pond = json!({ "shape": "fill", "color": "#86d5ed", "points": [[0, 0], [70, 0], [70, 70], [0, 70]] });
    let (status, _) = call(&app, As::Dm, "POST", &path, Some(pond)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let pond_id = view(&app, As::Dm, &t).await["scene"]["drawings"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let moved = json!({ "points": [[10, 0], [80, 0], [80, 70], [10, 70]] });
    let (status, _) = call(
        &app,
        As::Dm,
        "PATCH",
        &format!("{path}/{pond_id}"),
        Some(moved),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    call(&app, As::Dm, "DELETE", &format!("{path}/{pond_id}"), None).await;
    // Words on the map are a drawing too; only text has them.
    let words = json!({ "shape": "text", "color": "#f0f0f0", "points": [[70, 70], [70, 70]], "text": "Inn" });
    let (status, _) = call(&app, As::Dm, "POST", &path, Some(words.clone())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let mut wordless = words.clone();
    wordless.as_object_mut().unwrap().remove("text");
    let (status, _) = call(&app, As::Dm, "POST", &path, Some(wordless)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut worded_line = words;
    worded_line["shape"] = json!("line");
    let (status, _) = call(&app, As::Dm, "POST", &path, Some(worded_line)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        change(As::Dm, 0, json!({ "text": "Nope" })).await,
        StatusCode::BAD_REQUEST
    );
    let text = view(&app, As::Dm, &t).await["scene"]["drawings"][2]["id"]
        .as_str()
        .unwrap()
        .to_string();
    call(&app, As::Dm, "DELETE", &format!("{path}/{text}"), None).await;

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
    // Bringing everyone's view to a spot is for whoever runs the table only.
    let bring = json!({ "x": 10, "y": 10, "bring": true });
    let path = format!("/tables/{}/ping", t.id);
    let (status, _) = call(&app, As::Dm, "POST", &path, Some(bring.clone())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, As::Seat(&token), "POST", &path, Some(bring)).await;
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

#[tokio::test]
async fn a_co_dm_changes_anything_on_the_board() {
    let app = host();
    let t = table(&app).await;
    let (tam_seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let (wren_seat, wren) = join(&app, &t.invite, "Wren", "user-2").await;
    let cave = scene(&app, &t, "Cave").await;
    let at = format!("/tables/{}/scenes/{cave}", t.id);
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/seats/{wren_seat}/permissions", t.id),
        Some(json!({ "permissions": ["draw"] })),
    )
    .await;
    let made = call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/seats/{tam_seat}/role", t.id),
        Some(json!({ "role": "codm" })),
    )
    .await;
    assert_eq!(made.0, StatusCode::NO_CONTENT);
    let line = json!({ "shape": "line", "color": "#ff9090", "points": [[0, 0], [70, 70]] });
    let drew = call(
        &app,
        As::Seat(&wren),
        "POST",
        &format!("{at}/drawings"),
        Some(line),
    )
    .await;
    assert_eq!(drew.0, StatusCode::NO_CONTENT);
    let drawing = view(&app, As::Dm, &t).await["scene"]["drawings"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Wren's line: the co-DM recolours it, puts it on the DM's layer, then erases it.
    let path = format!("{at}/drawings/{drawing}");
    let change = json!({ "color": "#123456", "hidden": true });
    let (status, _) = call(&app, As::Seat(&tam), "PATCH", &path, Some(change)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let lighting = json!({
        "fog": { "mode": "dynamic", "light": "dark", "remember": true },
        "walls": [{ "id": "gate", "a": [700, 0], "b": [700, 70], "door": { "open": false } }],
        "lights": [],
    });
    let lit = call(
        &app,
        As::Seat(&tam),
        "PUT",
        &format!("{at}/lighting"),
        Some(lighting),
    )
    .await;
    assert_eq!(lit.0, StatusCode::NO_CONTENT);
    let open = json!({ "open": true });
    let door = call(
        &app,
        As::Seat(&tam),
        "POST",
        &format!("{at}/doors/gate"),
        Some(open),
    )
    .await;
    assert_eq!(door.0, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, As::Seat(&tam), "DELETE", &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let scene = view(&app, As::Dm, &t).await["scene"].clone();
    assert_eq!(scene["drawings"], json!([]));
    assert_eq!(scene["walls"][0]["door"]["open"], true);
}

#[tokio::test]
async fn only_whoever_runs_the_table_adds_hidden_tokens() {
    let app = host();
    let t = table(&app).await;
    let (seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let cave = scene(&app, &t, "Cave").await;
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/seats/{seat}/permissions", t.id),
        Some(json!({ "permissions": ["tokens"] })),
    )
    .await;
    let path = format!("/tables/{}/scenes/{cave}/tokens", t.id);
    let spy = json!({ "name": "Spy", "x": 1, "y": 1, "hidden": true });
    let (status, _) = call(&app, As::Seat(&tam), "POST", &path, Some(spy.clone())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, As::Dm, "POST", &path, Some(spy)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let theirs = view(&app, As::Seat(&tam), &t).await["scene"]["tokens"].clone();
    assert!(
        theirs
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["name"] != "Spy")
    );
    let mine = view(&app, As::Dm, &t).await["scene"]["tokens"].clone();
    assert!(
        mine.as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "Spy" && t["hidden"] == true)
    );
}

#[tokio::test]
async fn carries_attached_tokens_and_stacks_them() {
    let app = host();
    let t = table(&app).await;
    let cave = scene(&app, &t, "Cave").await;
    let path = format!("/tables/{}/scenes/{cave}/tokens", t.id);
    for (name, x) in [("Horse", 5), ("Rider", 5), ("Owl", 6)] {
        let token = json!({ "name": name, "x": x, "y": 5 });
        call(&app, As::Dm, "POST", &path, Some(token)).await;
    }
    let tokens = || {
        let app = app.clone();
        let t = &t;
        async move {
            view(&app, As::Dm, t).await["scene"]["tokens"]
                .as_array()
                .unwrap()
                .clone()
        }
    };
    let id_of = |all: &[Value], name: &str| {
        all.iter().find(|t| t["name"] == name).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let all = tokens().await;
    let (horse, rider, owl) = (
        id_of(&all, "Horse"),
        id_of(&all, "Rider"),
        id_of(&all, "Owl"),
    );
    let change = |id: &str, body: Value| {
        let (app, at) = (app.clone(), format!("{path}/{id}"));
        async move { call(&app, As::Dm, "PATCH", &at, Some(body)).await.0 }
    };
    assert_eq!(
        change(&rider, json!({ "attachedTo": horse })).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        change(&owl, json!({ "attachedTo": rider })).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        change(&horse, json!({ "attachedTo": owl })).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        change(&horse, json!({ "attachedTo": horse })).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        change(&horse, json!({ "x": 7, "y": 5 })).await,
        StatusCode::NO_CONTENT
    );
    let all = tokens().await;
    let at = |name: &str| {
        let t = all.iter().find(|t| t["name"] == name).unwrap();
        (t["x"].as_u64().unwrap(), t["y"].as_u64().unwrap())
    };
    assert_eq!((at("Rider"), at("Owl")), ((7, 5), (8, 5)));
    assert_eq!(
        change(&horse, json!({ "layer": "back" })).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(tokens().await[0]["name"], "Horse");
    assert_eq!(
        change(&horse, json!({ "layer": "sideways" })).await,
        StatusCode::BAD_REQUEST
    );
    let (status, _) = call(&app, As::Dm, "DELETE", &format!("{path}/{horse}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let all = tokens().await;
    let rider_now = all.iter().find(|t| t["name"] == "Rider").unwrap();
    assert!(rider_now.get("attachedTo").is_none());
}

#[tokio::test]
async fn keeps_how_diagonals_count() {
    let app = host();
    let t = table(&app).await;
    let cave = scene(&app, &t, "Cave").await;
    let grid = json!({ "size": 70, "offsetX": 0, "offsetY": 0, "feetPerCell": 5, "show": true, "diagonals": "alternate" });
    let setup = json!({ "setup": { "width": 1400, "height": 980, "grid": grid } });
    let path = format!("/tables/{}/scenes/{cave}", t.id);
    let (status, _) = call(&app, As::Dm, "PATCH", &path, Some(setup.clone())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        view(&app, As::Dm, &t).await["scene"]["grid"]["diagonals"],
        "alternate"
    );
    let mut streets = setup.clone();
    streets["setup"]["grid"]["diagonals"] = json!("manhattan");
    let (status, _) = call(&app, As::Dm, "PATCH", &path, Some(streets)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let mut odd = setup;
    odd["setup"]["grid"]["diagonals"] = json!("sideways");
    let (status, _) = call(&app, As::Dm, "PATCH", &path, Some(odd)).await;
    assert!(status.is_client_error());
}

#[tokio::test]
async fn keeps_the_grid_style() {
    let app = host();
    let t = table(&app).await;
    let cave = scene(&app, &t, "Cave").await;
    let grid = json!({ "size": 70, "offsetX": 0, "offsetY": 0, "feetPerCell": 5, "show": true,
        "style": { "color": "#1a2b3c", "width": 2, "opacity": 0.6, "dash": true } });
    let setup = json!({ "setup": { "width": 1400, "height": 980, "grid": grid } });
    let path = format!("/tables/{}/scenes/{cave}", t.id);
    let (status, _) = call(&app, As::Dm, "PATCH", &path, Some(setup.clone())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let view = view(&app, As::Dm, &t).await;
    assert_eq!(view["scene"]["grid"]["style"]["color"], "#1a2b3c");
    assert_eq!(view["scene"]["grid"]["style"]["dash"], true);
    let mut odd = setup.clone();
    odd["setup"]["grid"]["style"]["color"] = json!("blue");
    let (status, _) = call(&app, As::Dm, "PATCH", &path, Some(odd)).await;
    assert!(status.is_client_error());
    let mut wide = setup;
    wide["setup"]["grid"]["style"]["width"] = json!(9);
    let (status, _) = call(&app, As::Dm, "PATCH", &path, Some(wide)).await;
    assert!(status.is_client_error());
}

#[tokio::test]
async fn gathers_maps_under_folders() {
    let app = host();
    let t = table(&app).await;
    let cave = scene(&app, &t, "Cave").await;
    let (status, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Keep", "folder": "Dungeon" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(made["folder"], "Dungeon");
    let keep = made["id"].as_str().unwrap().to_string();
    let listed = |view: &Value, id: &str| {
        view["scenes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == json!(id))
            .unwrap()
            .clone()
    };
    let looking = view(&app, As::Dm, &t).await;
    assert_eq!(listed(&looking, &keep)["folder"], "Dungeon");
    assert!(listed(&looking, &cave)["folder"].is_null());
    // The map itself carries its folder; null moves it back out.
    let path = format!("/tables/{}/scenes/{keep}", t.id);
    let seen = call(
        &app,
        As::Dm,
        "GET",
        &format!("/tables/{}?scene={keep}", t.id),
        None,
    )
    .await
    .1;
    assert_eq!(seen["scene"]["folder"], "Dungeon");
    let (status, _) = call(
        &app,
        As::Dm,
        "PATCH",
        &path,
        Some(json!({ "folder": null })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let looking = view(&app, As::Dm, &t).await;
    assert!(listed(&looking, &keep)["folder"].is_null());
    let cave_path = format!("/tables/{}/scenes/{cave}", t.id);
    let (status, _) = call(
        &app,
        As::Dm,
        "PATCH",
        &cave_path,
        Some(json!({ "folder": "  " })),
    )
    .await;
    assert!(status.is_client_error());
    let (status, _) = call(
        &app,
        As::Dm,
        "PATCH",
        &cave_path,
        Some(json!({ "folder": "x".repeat(41) })),
    )
    .await;
    assert!(status.is_client_error());
}

#[tokio::test]
async fn marks_tokens_and_keeps_a_hidden_name_from_players() {
    let app = host();
    let t = table(&app).await;
    let (_, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let cave = scene(&app, &t, "Cave").await;
    let path = format!("/tables/{}/scenes/{cave}/tokens", t.id);
    call(
        &app,
        As::Dm,
        "POST",
        &path,
        Some(json!({ "name": "Vampire", "x": 4, "y": 4 })),
    )
    .await;
    let id = view(&app, As::Dm, &t).await["scene"]["tokens"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let at = format!("{path}/{id}");
    let marked = json!({ "nameHidden": true, "marks": ["red", "blue"] });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &at, Some(marked)).await.0,
        StatusCode::NO_CONTENT
    );
    let odd = json!({ "marks": ["pink"] });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &at, Some(odd)).await.0,
        StatusCode::BAD_REQUEST
    );
    // A number on it, which a player who may not change tokens can't touch.
    let counted = json!({ "counter": 12 });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &at, Some(counted)).await.0,
        StatusCode::NO_CONTENT
    );
    for odd in [json!({ "counter": 1.5 }), json!({ "counter": 5000 })] {
        assert_eq!(
            call(&app, As::Dm, "PATCH", &at, Some(odd)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let theirs_too = json!({ "counter": 3 });
    assert!(
        call(&app, As::Seat(&tam), "PATCH", &at, Some(theirs_too))
            .await
            .0
            .is_client_error()
    );
    let theirs = view(&app, As::Seat(&tam), &t).await["scene"]["tokens"][0].clone();
    assert_eq!(theirs["name"], "Unknown");
    assert_eq!(theirs["marks"], json!(["red", "blue"]));
    assert_eq!(theirs["counter"], 12);
    let cleared = json!({ "counter": null });
    call(&app, As::Dm, "PATCH", &at, Some(cleared)).await;
    assert!(
        view(&app, As::Dm, &t).await["scene"]["tokens"][0]
            .get("counter")
            .is_none()
    );
    assert_eq!(
        view(&app, As::Dm, &t).await["scene"]["tokens"][0]["name"],
        "Vampire"
    );
}

#[tokio::test]
async fn turns_and_mirrors_pictures() {
    let app = host();
    let t = table(&app).await;
    let cave = scene(&app, &t, "Cave").await;
    let path = format!("/tables/{}/scenes/{cave}/tokens", t.id);
    call(
        &app,
        As::Dm,
        "POST",
        &path,
        Some(json!({ "name": "Bat", "x": 2, "y": 2 })),
    )
    .await;
    let id = view(&app, As::Dm, &t).await["scene"]["tokens"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let at = format!("{path}/{id}");
    let turned = json!({ "rotation": 90, "flip": true });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &at, Some(turned)).await.0,
        StatusCode::NO_CONTENT
    );
    let odd = json!({ "rotation": 45 });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &at, Some(odd)).await.0,
        StatusCode::BAD_REQUEST
    );
    let bat = view(&app, As::Dm, &t).await["scene"]["tokens"][0].clone();
    assert_eq!(
        (bat["rotation"].clone(), bat["flip"].clone()),
        (json!(90), json!(true))
    );
    let back = json!({ "rotation": 0, "flip": false });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &at, Some(back)).await.0,
        StatusCode::NO_CONTENT
    );
    let bat = view(&app, As::Dm, &t).await["scene"]["tokens"][0].clone();
    assert!(bat.get("rotation").is_none() && bat.get("flip").is_none());
}

#[tokio::test]
async fn keeps_tokens_between_cells_and_tiny_ones() {
    let app = host();
    let t = table(&app).await;
    let cave = scene(&app, &t, "Cave").await;
    let path = format!("/tables/{}/scenes/{cave}/tokens", t.id);
    let rat = json!({ "name": "Rat", "x": 1.5, "y": 2.25, "size": 0.5 });
    assert_eq!(
        call(&app, As::Dm, "POST", &path, Some(rat)).await.0,
        StatusCode::NO_CONTENT
    );
    let odd = json!({ "name": "Odd", "x": 1, "y": 1, "size": 0.3 });
    assert_eq!(
        call(&app, As::Dm, "POST", &path, Some(odd)).await.0,
        StatusCode::BAD_REQUEST
    );
    let token = view(&app, As::Dm, &t).await["scene"]["tokens"][0].clone();
    assert_eq!(
        (
            token["x"].clone(),
            token["y"].clone(),
            token["size"].clone()
        ),
        (json!(1.5), json!(2.25), json!(0.5))
    );
    let at = format!("{path}/{}", token["id"].as_str().unwrap());
    let moved = json!({ "x": 3.333, "y": 2 });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &at, Some(moved)).await.0,
        StatusCode::NO_CONTENT
    );
    let token = view(&app, As::Dm, &t).await["scene"]["tokens"][0].clone();
    // Whole numbers stay whole, as the website sends them.
    assert_eq!(
        (token["x"].clone(), token["y"].clone()),
        (json!(3.33), json!(2))
    );
}

#[tokio::test]
async fn shares_rulers_with_anyone_and_trails_with_those_who_draw() {
    let app = host();
    let t = table(&app).await;
    let (_, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let path = format!("/tables/{}/mark", t.id);
    let ruler = json!({ "kind": "ruler", "points": [[35, 35], [175, 35]], "tab": "tab-1" });
    // No map shown yet: nothing to mark.
    let (status, _) = call(&app, As::Dm, "POST", &path, Some(ruler.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    scene(&app, &t, "Cave").await;
    let (status, _) = call(&app, As::Seat(&tam), "POST", &path, Some(ruler)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let trail = json!({ "kind": "trail", "points": [[10, 10], [20, 20]], "tab": "tab-1" });
    let (status, _) = call(&app, As::Seat(&tam), "POST", &path, Some(trail.clone())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, As::Dm, "POST", &path, Some(trail)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let many: Vec<[i32; 2]> = (0..60).map(|i| [i, i]).collect();
    let long = json!({ "kind": "ruler", "points": many, "tab": "tab-1" });
    let (status, _) = call(&app, As::Dm, "POST", &path, Some(long)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn pins_rulers_and_takes_them_off() {
    let app = host();
    let t = table(&app).await;
    let (_, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let cave = scene(&app, &t, "Cave").await;
    let path = format!("/tables/{}/scenes/{cave}/rulers", t.id);
    let points = json!([[35, 35], [245, 35]]);
    let (status, made) = call(
        &app,
        As::Dm,
        "POST",
        &path,
        Some(json!({ "points": points })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = made["id"].as_str().unwrap().to_string();
    let short = json!({ "points": [[1, 1]] });
    assert_eq!(
        call(&app, As::Dm, "POST", &path, Some(short)).await.0,
        StatusCode::BAD_REQUEST
    );
    let theirs = view(&app, As::Seat(&tam), &t).await["scene"]["rulers"].clone();
    // Spots on the map are kept as they came, in pixels.
    let kept = json!([[35.0, 35.0], [245.0, 35.0]]);
    assert_eq!(theirs, json!([{ "id": id, "points": kept, "by": "dm" }]));
    // A player who may not draw pins none, and takes none of the DM's off.
    let again = json!({ "points": points });
    assert!(
        call(&app, As::Seat(&tam), "POST", &path, Some(again))
            .await
            .0
            .is_client_error()
    );
    let one = format!("{path}/{id}");
    assert!(
        call(&app, As::Seat(&tam), "DELETE", &one, None)
            .await
            .0
            .is_client_error()
    );
    assert_eq!(
        call(&app, As::Dm, "DELETE", &one, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(view(&app, As::Dm, &t).await["scene"]["rulers"], json!([]));
}
