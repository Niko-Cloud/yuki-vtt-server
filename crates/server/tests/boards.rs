//! A board's map pictures, as the website has them: from the library, their squares matching
//! the board's, the board growing to hold them.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

// The smallest PNG there is, as far as the host can tell from its first bytes.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-a-map-picture";

#[tokio::test]
async fn lays_map_pictures_from_the_library_on_a_board() {
    let app = host();
    let t = table(&app).await;
    let (_, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    // Kept for the whole test, so the closures below can take Tam's seat as who calls.
    let tam: &'static str = Box::leak(tam.into_boxed_str());
    let player = As::Seat(tam);
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Board" })),
    )
    .await;
    let board = made["id"].as_str().unwrap().to_string();
    let at = format!("/tables/{}/scenes/{board}", t.id);
    let upload_as = |kind: &'static str, width: u32, height: u32| {
        let (app, path) = (
            app.clone(),
            format!(
                "/tables/{}/assets?kind={kind}&name=Crypt&width={width}&height={height}",
                t.id
            ),
        );
        async move {
            let (status, body) = upload(&app, As::Dm, "POST", &path, PNG).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body["id"].as_str().unwrap().to_string()
        }
    };
    // A map picture 1000 by 500 with 50-pixel squares: 20 by 10 of the board's 70.
    let crypt = upload_as("map", 1000, 500).await;
    let portrait = upload_as("token", 64, 64).await;
    let grid = |asset: String, cell: f64| {
        let (app, path) = (app.clone(), format!("/tables/{}/assets/{asset}", t.id));
        async move {
            call(
                &app,
                As::Dm,
                "PATCH",
                &path,
                Some(json!({ "grid": { "cell": cell, "offsetX": 0, "offsetY": 0 } })),
            )
            .await
            .0
        }
    };
    assert_eq!(grid(crypt.clone(), 50.0).await, StatusCode::OK);
    assert_eq!(grid(portrait.clone(), 50.0).await, StatusCode::BAD_REQUEST);
    let maps = format!("{at}/maps");
    let (refused, _) = call(
        &app,
        player,
        "POST",
        &maps,
        Some(json!({ "asset": crypt, "x": 0, "y": 0 })),
    )
    .await;
    assert_ne!(refused, StatusCode::OK);
    let (status, _) = call(
        &app,
        As::Dm,
        "POST",
        &maps,
        Some(json!({ "asset": portrait, "x": 0, "y": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Dropped near 600, 10: on the board's grid, sized to match, and the board grows.
    let (status, item) = call(
        &app,
        As::Dm,
        "POST",
        &maps,
        Some(json!({ "asset": crypt, "x": 600, "y": 10 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let item = item["id"].as_str().unwrap().to_string();
    let shown = |who: As<'static>| {
        let (app, path) = (app.clone(), format!("/tables/{}?scene={board}", t.id));
        async move { call(&app, who, "GET", &path, None).await.1 }
    };
    let view: Value = shown(As::Dm).await;
    assert_eq!(
        view["scene"]["maps"],
        json!([{ "id": item, "asset": crypt, "x": 630.0, "y": 0.0, "width": 1400.0, "height": 700.0 }])
    );
    assert_eq!(
        (
            view["scene"]["width"].as_u64(),
            view["scene"]["height"].as_u64()
        ),
        (Some(2030), Some(980))
    );
    // It comes unlocked; locked, it stays put until unlocked.
    let one = format!("{maps}/{item}");
    let lock = Some(json!({ "locked": true }));
    let (status, _) = call(&app, As::Dm, "PATCH", &one, lock).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, As::Dm, "PATCH", &one, Some(json!({ "x": 0 }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        As::Dm,
        "PATCH",
        &one,
        Some(json!({ "x": 0, "locked": false, "rotation": 90 })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Dragged two squares past the left edge: the board grows there, a token along with it.
    let ogre = Some(json!({ "name": "Ogre", "x": 1, "y": 1 }));
    call(&app, As::Dm, "POST", &format!("{at}/tokens"), ogre).await;
    let grow = format!("{at}/grow");
    let two = || Some(json!({ "columns": 2, "rows": 0, "maps": [{ "id": item, "x": 0, "y": 0 }] }));
    let (status, _) = call(&app, player, "POST", &grow, two()).await;
    assert_ne!(status, StatusCode::NO_CONTENT);
    let none = Some(json!({ "columns": 0, "rows": 0 }));
    let (status, _) = call(&app, As::Dm, "POST", &grow, none).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, As::Dm, "POST", &grow, two()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let view = shown(As::Dm).await;
    assert_eq!(view["scene"]["width"], 2170);
    assert_eq!(view["scene"]["maps"][0]["x"], 0.0);
    let ogre = view["scene"]["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Ogre")
        .unwrap();
    assert_eq!(ogre["x"], 3);
    // Lined up anew (25-pixel squares): it doubles on every board.
    assert_eq!(grid(crypt.clone(), 25.0).await, StatusCode::OK);
    let view = shown(As::Dm).await;
    assert_eq!(view["scene"]["maps"][0]["width"], 2800.0);
    // Players get the picture with the board they see.
    call(&app, As::Dm, "POST", &format!("{at}/show"), None).await;
    let theirs = shown(player).await;
    assert!(
        theirs["assets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == crypt.as_str())
    );
    // Deleted from the library, it comes off the board.
    call(
        &app,
        As::Dm,
        "DELETE",
        &format!("/tables/{}/assets/{crypt}", t.id),
        None,
    )
    .await;
    assert_eq!(shown(As::Dm).await["scene"]["maps"], json!([]));
}

#[tokio::test]
async fn turns_maps_made_before_boards_into_boards_once() {
    let (app, state) = host_and_state();
    let t = table(&app).await;
    let scene = |name: &'static str| {
        let (app, path) = (app.clone(), format!("/tables/{}/scenes", t.id));
        async move {
            let (_, made) = call(&app, As::Dm, "POST", &path, Some(json!({ "name": name }))).await;
            made["id"].as_str().unwrap().to_string()
        }
    };
    let (cave, room) = (scene("Cave").await, scene("Room").await);
    // Kept the old way: each map's own picture in Library/Maps, and the room (3 by 2 squares)
    // laid inside the cave as a piece.
    {
        let db = state.db();
        let data_dir = &state.config.data_dir;
        let image = json!({ "mime": "image/png", "version": 1 });
        for (map, name, width, height) in [(&cave, "Cave", 700, 490), (&room, "Room", 210, 140)] {
            let file = yuki_vtt_server::library::place_map(data_dir, name, "image/png");
            yuki_vtt_server::library::put(data_dir, &file, PNG).unwrap();
            let stored: String = db
                .query_row("SELECT data FROM scenes WHERE id = ?1", [map], |row| {
                    row.get(0)
                })
                .unwrap();
            let mut data: Value = serde_json::from_str(&stored).unwrap();
            data["width"] = json!(width);
            data["height"] = json!(height);
            data["image"] = image.clone();
            if map == &cave {
                data["pieces"] = json!([{
                    "id": "piece-1", "map": room, "image": image,
                    "x": 3, "y": 2, "w": 3.0, "h": 2.0
                }]);
            }
            db.execute(
                "UPDATE scenes SET data = ?2, file = ?3 WHERE id = ?1",
                rusqlite::params![map, data.to_string(), file],
            )
            .unwrap();
        }
    }
    let convert =
        || yuki_vtt_server::boards::convert_old_maps(&state.config.data_dir, &state.db()).unwrap();
    assert_eq!(convert(), 3);
    let shown = call(
        &app,
        As::Dm,
        "GET",
        &format!("/tables/{}?scene={cave}", t.id),
        None,
    )
    .await
    .1;
    let board = &shown["scene"];
    assert!(board.get("image").is_none());
    assert!(board.get("pieces").is_none());
    // Its own picture where it was, and the piece where it lay, both from the library.
    let placed: Vec<(f64, f64, f64)> = board["maps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            let at = |key: &str| m[key].as_f64().unwrap();
            (at("x"), at("y"), at("width"))
        })
        .collect();
    assert_eq!(placed, [(0.0, 0.0, 700.0), (210.0, 140.0, 210.0)]);
    let own = board["maps"][0]["asset"].as_str().unwrap();
    let picture = shown["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == own)
        .unwrap();
    assert_eq!(picture["kind"], "map");
    assert_eq!(picture["name"], "Cave");
    assert_eq!(
        picture["grid"],
        json!({ "cell": 70.0, "offsetX": 0.0, "offsetY": 0.0 })
    );
    let (status, _) = call(
        &app,
        As::Dm,
        "GET",
        &format!("/tables/{}/assets/{own}", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Already boards: nothing more to do.
    assert_eq!(convert(), 0);
}

#[tokio::test]
async fn scales_map_pictures_with_the_boards_squares() {
    let app = host();
    let t = table(&app).await;
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Board" })),
    )
    .await;
    let board = made["id"].as_str().unwrap().to_string();
    let picture = laid_picture(&app, &t, &board, PNG, (700, 490)).await;
    // The boards list shows it as the board's thumbnail.
    let shown = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None)
        .await
        .1;
    let listed = shown["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == board.as_str());
    assert_eq!(listed.unwrap()["picture"], picture.as_str());
    let at = format!("/tables/{}/scenes/{board}", t.id);
    // Squares half as big: 20 by 14 of them still, and the picture on them half its size.
    let grid = json!({ "size": 35, "offsetX": 0, "offsetY": 0, "feetPerCell": 5, "show": true });
    let setup = json!({ "setup": { "width": 700, "height": 490, "grid": grid } });
    let (status, body) = call(&app, As::Dm, "PATCH", &at, Some(setup)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let shown = call(
        &app,
        As::Dm,
        "GET",
        &format!("/tables/{}?scene={board}", t.id),
        None,
    )
    .await
    .1;
    let item = &shown["scene"]["maps"][0];
    assert_eq!(
        (item["width"].as_f64(), item["height"].as_f64()),
        (Some(350.0), Some(245.0))
    );
}
