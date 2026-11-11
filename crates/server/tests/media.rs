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

#[tokio::test]
async fn gives_a_player_a_pass_for_their_own_seat_only() {
    let app = host();
    let t = table(&app).await;
    let (seat, token) = join(&app, &t.invite, "Wren", "player-1").await;
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
    call(
        &app,
        As::Dm,
        "PUT",
        &format!("/tables/{}/handout", t.id),
        Some(serde_json::json!({ "assetId": letter, "title": "A letter" })),
    )
    .await;
    let pass = seat_pass(&app, &t.id, &token).await;
    assert!(!pass.contains(&token));
    let picture = format!("/tables/{}/assets/{letter}?pass={pass}", t.id);
    assert_eq!(fetch(&app, &picture).await.0, StatusCode::OK);

    // Only for this table, never by the seat token in the address, and not for a stranger.
    let other = table(&app).await;
    let (status, _, _) = fetch(
        &app,
        &format!("/tables/{}/assets/{letter}?pass={pass}", other.id),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        As::Nobody,
        "POST",
        &format!("/tables/{}/seat-pass", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "POST",
        &format!("/tables/{}/seat-pass", other.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A seat's pass doesn't make it the DM, and a removed seat's pass stops working.
    let forged = pass.replacen(&seat, "", 1);
    assert_eq!(
        fetch(
            &app,
            &format!("/tables/{}/assets/{letter}?pass={forged}", t.id)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    call(
        &app,
        As::Dm,
        "DELETE",
        &format!("/tables/{}/seats/{seat}", t.id),
        None,
    )
    .await;
    assert_eq!(fetch(&app, &picture).await.0, StatusCode::NOT_FOUND);
}
