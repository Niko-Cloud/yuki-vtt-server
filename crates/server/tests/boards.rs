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
        json!([{ "id": item, "asset": crypt, "x": 630.0, "y": 0.0, "width": 1400.0, "height": 700.0, "locked": true }])
    );
    assert_eq!(
        (
            view["scene"]["width"].as_u64(),
            view["scene"]["height"].as_u64()
        ),
        (Some(2030), Some(980))
    );
    // Locked, it stays put until unlocked.
    let one = format!("{maps}/{item}");
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
