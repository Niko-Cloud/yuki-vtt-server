//! The host's routes, called the way the website's pages call them.

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

const KEY: &str = "the-dms-key";
const SECRET: &[u8] = b"shared-with-the-website";
const SITE: &str = "https://yuki.test";

fn host() -> Router {
    let config = Config {
        owner_key_hash: ids::hash(KEY),
        website_secret: Some(SECRET.to_vec()),
        allowed_origins: vec![SITE.into()],
        data_dir: std::env::temp_dir(),
    };
    app(AppState::new(db::in_memory().unwrap(), config))
}

/// Who is calling: the DM, a seat, or nobody.
#[derive(Clone, Copy)]
enum As<'a> {
    Dm,
    Seat(&'a str),
    Nobody,
}

async fn call(
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

// A character as the website hands it over: the snapshot text and its signature.
fn signed(name: &str, player: &str, armor_class: i64, state: Value) -> Value {
    let snapshot = json!({
        "playerId": player,
        "summary": { "name": name, "classes": "Fighter 3", "armorClass": armor_class, "initiative": 2, "maxHitPoints": 28, "edition": "2014" },
        "character": { "name": name, "state": state },
        "sheet": { "armorClass": { "value": armor_class } },
        "play": { "hitPoints": { "current": 28, "max": 28, "temporary": 0 } },
    })
    .to_string();
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
    mac.update(snapshot.as_bytes());
    json!({ "snapshot": snapshot, "signature": hex::encode(mac.finalize().into_bytes()) })
}

fn fresh() -> Value {
    json!({ "currentHitPoints": null, "temporaryHitPoints": 0, "conditions": [] })
}

struct Table {
    id: String,
    invite: String,
}

async fn table(app: &Router) -> Table {
    let (status, made) = call(
        app,
        As::Dm,
        "POST",
        "/tables",
        Some(json!({ "name": "Tuesday group", "dmName": "Yuki" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    Table {
        id: made["id"].as_str().unwrap().into(),
        invite: made["inviteToken"].as_str().unwrap().into(),
    }
}

async fn join(app: &Router, invite: &str, name: &str, player: &str) -> (String, String) {
    let (status, seat) = call(
        app,
        As::Nobody,
        "POST",
        &format!("/join/{invite}"),
        Some(signed(name, player, 18, fresh())),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{seat}");
    (
        seat["seatId"].as_str().unwrap().into(),
        seat["seatToken"].as_str().unwrap().into(),
    )
}

#[tokio::test]
async fn only_the_dms_key_makes_and_lists_tables() {
    let app = host();
    let made = table(&app).await;
    let (status, _) = call(
        &app,
        As::Nobody,
        "POST",
        "/tables",
        Some(json!({ "name": "Mine now" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, list) = call(&app, As::Dm, "GET", "/tables", None).await;
    assert_eq!(list[0]["id"], made.id.as_str());
    assert_eq!(list[0]["seats"], 0);
    let (status, _) = call(&app, As::Nobody, "GET", "/tables", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn seats_a_signed_character_and_shows_the_table() {
    let app = host();
    let t = table(&app).await;
    let (_, invite) = call(
        &app,
        As::Nobody,
        "GET",
        &format!("/join/{}", t.invite),
        None,
    )
    .await;
    assert_eq!(invite["party"]["name"], "Tuesday group");
    assert_eq!(invite["party"]["dm"], "Yuki");

    let (seat, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let (status, view) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["you"]["role"], "player");
    assert_eq!(view["you"]["seatIds"], json!([seat]));
    assert_eq!(view["you"]["permissions"], json!(["move-own"]));
    assert_eq!(view["members"][0]["name"], "Tam");
    assert_eq!(
        view["members"][0]["hitPoints"],
        json!({ "current": 28, "max": 28, "temporary": 0 })
    );
    assert_eq!(view["members"][0]["armorClass"], 18);

    // Strangers learn nothing; a forged sheet doesn't sit down.
    let (status, _) = call(&app, As::Nobody, "GET", &format!("/tables/{}", t.id), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let mut forged = signed("Tam", "user-2", 18, fresh());
    forged["snapshot"] = json!(forged["snapshot"].as_str().unwrap().replace("18", "30"));
    let (status, _) = call(
        &app,
        As::Nobody,
        "POST",
        &format!("/join/{}", t.invite),
        Some(forged),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn keeps_hidden_rolls_to_the_dm_and_a_private_roll_to_its_player() {
    let app = host();
    let t = table(&app).await;
    let (tam_seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let (_, wren) = join(&app, &t.invite, "Wren", "user-2").await;
    let roll = |label: &str, hidden: bool, seat: Option<&str>| json!({ "label": label, "expression": "1d20+4", "hidden": hidden, "seatId": seat });

    let (status, result) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/roll", t.id),
        Some(roll("Ambush", true, None)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let total = result["total"].as_i64().unwrap();
    assert!((5..=24).contains(&total));
    call(
        &app,
        As::Seat(&tam),
        "POST",
        &format!("/tables/{}/roll", t.id),
        Some(roll("Insight", true, Some(&tam_seat))),
    )
    .await;
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/roll", t.id),
        Some(roll("Thunder", false, None)),
    )
    .await;

    let labels = |view: &Value| {
        view["log"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["data"]["label"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let (_, dm_view) = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None).await;
    assert_eq!(labels(&dm_view), ["Ambush", "Insight", "Thunder"]);
    assert_eq!(dm_view["log"][0]["data"]["dm"], true);
    assert_eq!(dm_view["log"][0]["author"], "Yuki");
    assert_eq!(dm_view["log"][1]["author"], "Tam");
    let (_, tam_view) = call(
        &app,
        As::Seat(&tam),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(labels(&tam_view), ["Insight", "Thunder"]);
    let (_, wren_view) = call(
        &app,
        As::Seat(&wren),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(labels(&wren_view), ["Thunder"]);

    let ambush = dm_view["log"][0]["id"].as_str().unwrap();
    let (status, _) = call(
        &app,
        As::Seat(&tam),
        "POST",
        &format!("/tables/{}/log/{ambush}/reveal", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/log/{ambush}/reveal", t.id),
        None,
    )
    .await;
    let (_, wren_view) = call(
        &app,
        As::Seat(&wren),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(labels(&wren_view), ["Ambush", "Thunder"]);

    // A roll for someone else's seat, or one that isn't dice, is refused.
    let (status, _) = call(
        &app,
        As::Seat(&wren),
        "POST",
        &format!("/tables/{}/roll", t.id),
        Some(roll("Steal", false, Some(&tam_seat))),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/roll", t.id),
        Some(json!({ "label": "Big", "expression": "9999d6" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_dms_damage_reaches_the_players_sheet_and_survives_their_save() {
    let app = host();
    let t = table(&app).await;
    let (seat, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let (status, member) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/seats/{seat}/change", t.id),
        Some(json!({ "kind": "damage", "amount": 10 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(member["hitPoints"]["current"], 18);
    let (_, pulled) = call(&app, As::Nobody, "GET", &format!("/seats/{token}"), None).await;
    assert_eq!(pulled["state"]["currentHitPoints"], 18);

    // The player spent a slot from the old state; the DM's damage stays.
    let mut theirs = fresh();
    theirs["spellSlotsLeft"] = json!([1]);
    let push = |state: Value| {
        let mut body = signed("Tam", "user-1", 18, state);
        body["playBase"] = fresh();
        body
    };
    let (_, saved) = call(
        &app,
        As::Nobody,
        "PUT",
        &format!("/seats/{token}"),
        Some(push(theirs)),
    )
    .await;
    assert_eq!(saved["state"]["currentHitPoints"], 18);
    assert_eq!(saved["state"]["spellSlotsLeft"], json!([1]));

    // The character panel shows the hit points as they are now.
    let (_, sheet) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}/seats/{seat}", t.id),
        None,
    )
    .await;
    assert_eq!(sheet["play"]["hitPoints"]["current"], 18);
    assert_eq!(sheet["sheet"]["armorClass"]["value"], 18);
}

#[tokio::test]
async fn runs_a_fight_for_the_dm_only() {
    let app = host();
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "POST",
        &format!("/tables/{}/encounter", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/encounter", t.id),
        None,
    )
    .await;
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/encounter/combatants", t.id),
        Some(json!({ "name": "Ogre", "initiative": 30 })),
    )
    .await;
    let (_, view) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(view["encounter"]["combatants"][0]["name"], "Ogre");
    assert_eq!(view["encounter"]["round"], 1);
    for _ in 0..2 {
        call(
            &app,
            As::Dm,
            "POST",
            &format!("/tables/{}/encounter/next", t.id),
            None,
        )
        .await;
    }
    let (_, view) = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None).await;
    assert_eq!(view["encounter"]["round"], 2);
    let notes: Vec<&str> = view["log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["data"]["text"].as_str().unwrap())
        .collect();
    assert!(notes[0].starts_with("Combat starts. Initiative: Tam "));
    assert_eq!(
        notes[1..],
        ["Ogre joins the fight (initiative 30).", "Round 2."]
    );
    call(
        &app,
        As::Dm,
        "DELETE",
        &format!("/tables/{}/encounter", t.id),
        None,
    )
    .await;
    let (_, view) = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None).await;
    assert_eq!(view["encounter"], Value::Null);
}

#[tokio::test]
async fn locks_bans_and_makes_co_dms() {
    let app = host();
    let t = table(&app).await;
    let (seat, token) = join(&app, &t.invite, "Tam", "user-1").await;

    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/seats/{seat}/role", t.id),
        Some(json!({ "role": "codm" })),
    )
    .await;
    let (_, view) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(view["you"]["role"], "codm");
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "POST",
        &format!("/tables/{}/encounter", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // A co-DM still can't hand out roles.
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "PATCH",
        &format!("/tables/{}/seats/{seat}/role", t.id),
        Some(json!({ "role": "player" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/lock", t.id),
        Some(json!({ "locked": true })),
    )
    .await;
    let (status, _) = call(
        &app,
        As::Nobody,
        "POST",
        &format!("/join/{}", t.invite),
        Some(signed("Wren", "user-2", 14, fresh())),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/lock", t.id),
        Some(json!({ "locked": false })),
    )
    .await;

    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/seats/{seat}/ban", t.id),
        None,
    )
    .await;
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        As::Nobody,
        "POST",
        &format!("/join/{}", t.invite),
        Some(signed("Tam again", "user-1", 18, fresh())),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    call(
        &app,
        As::Dm,
        "DELETE",
        &format!("/tables/{}/bans/user-1", t.id),
        None,
    )
    .await;
    join(&app, &t.invite, "Tam again", "user-1").await;

    // A new invite retires the old link.
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/invite", t.id),
        None,
    )
    .await;
    let (status, _) = call(
        &app,
        As::Nobody,
        "GET",
        &format!("/join/{}", t.invite),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn lets_only_the_paired_website_call_from_a_browser() {
    let app = host();
    let preflight = |origin: &str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/api/tables")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "authorization, content-type",
            )
            .body(Body::empty())
            .unwrap()
    };
    let allowed = app.clone().oneshot(preflight(SITE)).await.unwrap();
    assert_eq!(allowed.headers()["access-control-allow-origin"], SITE);
    let other = app
        .clone()
        .oneshot(preflight("https://evil.test"))
        .await
        .unwrap();
    assert!(other.headers().get("access-control-allow-origin").is_none());
}
