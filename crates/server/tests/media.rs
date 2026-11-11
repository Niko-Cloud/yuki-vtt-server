//! The DM's media pass: pictures and the live channel by address, without the DM's key.

mod common;

use axum::http::StatusCode;
use common::*;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-not-really-but-the-header-is-right";

async fn pass_for(app: &axum::Router, table: &str) -> String {
    let (status, given) = call(
        app,
        As::Dm,
        "POST",
        &format!("/tables/{table}/media-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(given["expiresInSeconds"], 3600);
    given["pass"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn lets_the_dms_pages_load_pictures_with_a_pass() {
    let app = host();
    let t = table(&app).await;
    let (_, letter) = upload(
        &app,
        As::Dm,
        "POST",
        &format!(
            "/tables/{}/assets?kind=handout&name=Letter&width=300&height=200",
            t.id
        ),
        PNG,
    )
    .await;
    let letter = letter["id"].as_str().unwrap();
    let picture = format!("/tables/{}/assets/{letter}", t.id);
    let pass = pass_for(&app, &t.id).await;

    let (status, kind, _) = fetch(&app, &format!("{picture}?pass={pass}")).await;
    assert_eq!((status, kind.as_str()), (StatusCode::OK, "image/png"));

    // The DM's key no longer goes in an address, and a pass is no good changed or elsewhere.
    let (status, _, _) = fetch(&app, &format!("{picture}?key={KEY}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let changed = format!("{}0", &pass[..pass.len() - 1]);
    let (status, _, _) = fetch(&app, &format!("{picture}?pass={changed}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = fetch(&app, &format!("{picture}?pass=4102444800000.00")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let other = table(&app).await;
    let (status, _, _) = fetch(
        &app,
        &format!("/tables/{}/assets/{letter}?pass={pass}", other.id),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn hands_out_passes_only_to_the_dm() {
    let app = host();
    let t = table(&app).await;
    let (_, player) = join(&app, &t.invite, "Wren", "player-1").await;
    let path = format!("/tables/{}/media-token", t.id);
    let (status, _) = call(&app, As::Seat(&player), "POST", &path, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&app, As::Nobody, "POST", &path, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&app, As::Dm, "POST", "/tables/nope/media-token", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A pass only reads: it can't do what the DM's key does.
    let pass = pass_for(&app, &t.id).await;
    let (status, _) = call(
        &app,
        As::Nobody,
        "POST",
        &format!("/tables/{}/roll?pass={pass}", t.id),
        Some(serde_json::json!({ "label": "Luck", "expression": "1d20" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
