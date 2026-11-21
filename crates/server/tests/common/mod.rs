//! What the route tests share: a host in memory, and calls made as the DM, a seat or nobody.
#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use hmac::{Hmac, Mac};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt;
use yuki_vtt_server::state::{AppState, Config};
use yuki_vtt_server::{app, db, ids};

pub const KEY: &str = "the-dms-key";
pub const SECRET: &[u8] = b"shared-with-the-website";
pub const SITE: &str = "https://yuki.test";

pub fn host() -> Router {
    host_and_state().0
}

/// A host, with its state to listen on its live channel.
pub fn host_and_state() -> (Router, AppState) {
    let config = Config {
        owner_key_hash: ids::hash(KEY),
        website_secret: Some(SECRET.to_vec()),
        allowed_origins: vec![SITE.into()],
        // Its own folder, kept until the test run ends.
        data_dir: tempfile::tempdir().unwrap().keep(),
    };
    let state = AppState::new(db::in_memory().unwrap(), config);
    (app(state.clone()), state)
}

/// Who is calling: the DM, a seat, or nobody.
#[derive(Clone, Copy)]
pub enum As<'a> {
    Dm,
    Seat(&'a str),
    Nobody,
}

pub async fn call(
    app: &Router,
    who: As<'_>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/api{path}"))
        .header("content-type", "application/json");
    request = match who {
        As::Dm => request.header("authorization", format!("Bearer {KEY}")),
        As::Seat(token) => request.header("x-seat-token", token),
        As::Nobody => request,
    };
    let body = body
        .map(|b| Body::from(b.to_string()))
        .unwrap_or_else(Body::empty);
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// What the website signs a copy for: joining a table, or saving a seat.
#[derive(Clone, Copy)]
pub enum For<'a> {
    Join(&'a str),
    Seat(&'a str),
}

// Each copy is signed a moment after the one before, as the website's clock moves on.
static SIGNED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

pub fn issued_now() -> i64 {
    ids::now() + SIGNED.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// A character as the website hands it over: the snapshot text and its signature.
pub fn signed(name: &str, player: &str, armor_class: i64, state: Value, signed_for: For) -> Value {
    let mut snapshot = json!({
        "playerId": player,
        "issuedAt": issued_now(),
        "summary": { "name": name, "classes": "Fighter 3", "armorClass": armor_class, "initiative": 2, "maxHitPoints": 28, "edition": "2014" },
        "character": { "name": name, "state": state },
        "member": { "name": name, "level": 3, "classes": "Fighter 3", "passive": { "perception": 13, "insight": 11, "investigation": 10 }, "hitPoints": { "current": 28, "max": 28, "temporary": 0 }, "conditions": [] },
        "sheet": { "armorClass": { "value": armor_class } },
        "play": { "hitPoints": { "current": 28, "max": 28, "temporary": 0 } },
    });
    match signed_for {
        For::Join(table) => snapshot["tableId"] = json!(table),
        For::Seat(seat) => snapshot["seatId"] = json!(seat),
    }
    sign(&snapshot.to_string())
}

/// The website's signature over exactly this text.
pub fn sign(snapshot: &str) -> Value {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
    mac.update(snapshot.as_bytes());
    json!({ "snapshot": snapshot, "signature": hex::encode(mac.finalize().into_bytes()) })
}

pub fn fresh() -> Value {
    json!({ "currentHitPoints": null, "temporaryHitPoints": 0, "conditions": [] })
}

pub struct Table {
    pub id: String,
    pub invite: String,
}

pub async fn table(app: &Router) -> Table {
    let (status, made) = call(
        app,
        As::Dm,
        "POST",
        "/tables",
        Some(json!({ "name": "Tuesday group", "dmName": "Yuki" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = made["id"].as_str().unwrap();
    // These tests seat players at once; asking to join has its own tests.
    call(
        app,
        As::Dm,
        "PATCH",
        &format!("/tables/{id}/ask"),
        Some(json!({ "askToJoin": false })),
    )
    .await;
    Table {
        id: made["id"].as_str().unwrap().into(),
        invite: made["inviteToken"].as_str().unwrap().into(),
    }
}

/// The table an invite is for, as the website learns it before signing a join.
pub async fn table_of(app: &Router, invite: &str) -> String {
    let (_, invited) = call(app, As::Nobody, "GET", &format!("/join/{invite}"), None).await;
    invited["party"]["id"].as_str().unwrap().into()
}

pub async fn join(app: &Router, invite: &str, name: &str, player: &str) -> (String, String) {
    let table = table_of(app, invite).await;
    let (status, seat) = call(
        app,
        As::Nobody,
        "POST",
        &format!("/join/{invite}"),
        Some(signed(name, player, 18, fresh(), For::Join(&table))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{seat}");
    (
        seat["seatId"].as_str().unwrap().into(),
        seat["seatToken"].as_str().unwrap().into(),
    )
}

/// Sends a file as the body, the way the website uploads pictures.
pub async fn upload(
    app: &Router,
    who: As<'_>,
    method: &str,
    path: &str,
    bytes: &[u8],
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/api{path}"))
        .header("content-type", "image/png");
    request = match who {
        As::Dm => request.header("authorization", format!("Bearer {KEY}")),
        As::Seat(token) => request.header("x-seat-token", token),
        As::Nobody => request,
    };
    let response = app
        .clone()
        .oneshot(request.body(Body::from(bytes.to_vec())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A seat's media pass, asked for with the seat token in a header, for addresses.
pub async fn seat_pass(app: &Router, table: &str, token: &str) -> String {
    let (status, given) = call(
        app,
        As::Seat(token),
        "POST",
        &format!("/tables/{table}/seat-pass"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{given}");
    given["pass"].as_str().unwrap().to_string()
}

/// Fetches a picture by its address, as an <img> does: no headers, the seat in the query.
pub async fn fetch(app: &Router, path: &str) -> (StatusCode, String, usize) {
    let request = Request::builder()
        .uri(format!("/api{path}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let kind = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, kind, bytes.len())
}
