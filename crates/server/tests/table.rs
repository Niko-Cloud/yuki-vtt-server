//! The host's routes, called the way the website's pages call them.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::{Value, json};
use tower::ServiceExt;

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
    let mut forged = signed("Tam", "user-2", 18, fresh(), For::Join(&t.id));
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
async fn a_tv_screen_sits_down_with_no_character() {
    let app = host();
    let t = table(&app).await;
    let (status, screen) = call(
        &app,
        As::Nobody,
        "POST",
        &format!("/join/{}/screen", t.invite),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{screen}");
    let token = screen["seatToken"].as_str().unwrap();
    assert_eq!(screen["partyId"], t.id.as_str());

    // The screen sees the table as a player with no rights, and is told it is a screen.
    let (status, view) = call(
        &app,
        As::Seat(token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["you"]["screen"], true);
    assert_eq!(view["you"]["role"], "player");
    assert_eq!(view["you"]["permissions"], json!([]));
    // A screen keeps no sheet, so it is not among the members the table reads.
    assert!(view["members"].as_array().unwrap().is_empty());
    // A player never receives the DM's list of screens.
    assert!(view["screens"].as_array().unwrap().is_empty());

    // The DM sees the screen in its own list, to remove it.
    let (_, dm) = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None).await;
    assert_eq!(dm["screens"][0]["seatId"], screen["seatId"]);
    assert_eq!(dm["screens"][0]["name"], "Screen");

    // The DM removes the screen; its seat is gone.
    let (status, _) = call(
        &app,
        As::Dm,
        "DELETE",
        &format!(
            "/tables/{}/seats/{}",
            t.id,
            screen["seatId"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &app,
        As::Seat(token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert!(status.is_client_error());
}

#[tokio::test]
async fn anyone_presses_the_x_card_and_no_name_is_shown() {
    let app = host();
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "POST",
        &format!("/tables/{}/xcard", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Everyone at the table sees an anonymous X-card line in the log.
    let (_, view) = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None).await;
    let last = view["log"].as_array().unwrap().last().unwrap();
    assert_eq!(last["kind"], "xcard");
    assert_eq!(last["author"], "");
    assert_eq!(last["data"]["text"], "X-card");

    // A stranger cannot press it.
    let (status, _) = call(
        &app,
        As::Nobody,
        "POST",
        &format!("/tables/{}/xcard", t.id),
        None,
    )
    .await;
    assert!(status.is_client_error());
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
    let (_, pulled) = call(&app, As::Seat(&token), "GET", "/seat", None).await;
    assert_eq!(pulled["state"]["currentHitPoints"], 18);

    // The player spent a slot from the old state; the DM's damage stays.
    let mut theirs = fresh();
    theirs["spellSlotsLeft"] = json!([1]);
    let push = |state: Value| {
        let mut body = signed("Tam", "user-1", 18, state, For::Seat(&seat));
        body["playBase"] = fresh();
        body
    };
    let (_, saved) = call(&app, As::Seat(&token), "PUT", "/seat", Some(push(theirs))).await;
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
        Some(signed("Wren", "user-2", 14, fresh(), For::Join(&t.id))),
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
        Some(signed("Tam again", "user-1", 18, fresh(), For::Join(&t.id))),
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

#[tokio::test]
async fn gives_the_dm_view_every_character_as_it_is_now() {
    let app = host();
    let t = table(&app).await;
    let (seat, token) = join(&app, &t.invite, "Tam", "user-1").await;
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/seats/{seat}/change", t.id),
        Some(json!({ "kind": "damage", "amount": 9 })),
    )
    .await;
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/seats/{seat}/change", t.id),
        Some(json!({ "kind": "conditions", "conditions": ["prone"], "exhaustion": 1 })),
    )
    .await;

    let (status, party) = call(
        &app,
        As::Dm,
        "GET",
        &format!("/tables/{}/party", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(party["you"], "dm");
    let tam = &party["members"][0];
    assert_eq!(
        (
            tam["seatId"].as_str(),
            tam["account"].as_bool(),
            tam["role"].as_str()
        ),
        (Some(seat.as_str()), Some(true), Some("player"))
    );
    // The website's summary, with the DM's changes since.
    assert_eq!(tam["member"]["level"], 3);
    assert_eq!(tam["member"]["passive"]["perception"], 13);
    assert_eq!(tam["member"]["hitPoints"]["current"], 19);
    assert_eq!(tam["member"]["conditions"], json!(["prone"]));
    assert_eq!(tam["member"]["exhaustion"], 1);
    // Players don't get the DM's view.
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}/party", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A ban is listed by the character that was played.
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/seats/{seat}/ban", t.id),
        None,
    )
    .await;
    let (_, party) = call(
        &app,
        As::Dm,
        "GET",
        &format!("/tables/{}/party", t.id),
        None,
    )
    .await;
    assert_eq!(party["members"], json!([]));
    assert_eq!(party["banned"], json!([{ "id": "user-1", "name": "Tam" }]));
}

#[tokio::test]
async fn slows_down_someone_hammering_the_invite() {
    let app = host();
    let t = table(&app).await;
    let mut refused = None;
    for _ in 0..25 {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/join/{}", t.invite))
            .header("content-type", "application/json")
            // Through the tunnel: Cloudflare says who it is.
            .header("cf-connecting-ip", "203.0.113.9")
            // Unsigned, so none of them takes a seat.
            .body(Body::from(
                json!({ "snapshot": "{}", "signature": "" }).to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            refused = Some(response);
            break;
        }
    }
    let refused = refused.expect("the host to say no before 25 joins");
    assert_eq!(refused.headers()["retry-after"], "60");
    // Someone else still gets in.
    join(&app, &t.invite, "Tam", "user-1").await;
}

#[tokio::test]
async fn keeps_monsters_numbers_for_the_dm_and_counts_effects_down() {
    let app = host();
    let t = table(&app).await;
    let (_, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let fight = format!("/tables/{}/encounter", t.id);
    call(&app, As::Dm, "POST", &fight, None).await;
    let ogre = json!({ "name": "Ogre", "bonus": -1, "initiative": 40, "hp": 30, "ac": 11 });
    let added = call(
        &app,
        As::Dm,
        "POST",
        &format!("{fight}/combatants"),
        Some(ogre),
    )
    .await;
    assert_eq!(added.0, StatusCode::NO_CONTENT);
    let ogre_of = |who| {
        let app = app.clone();
        let path = format!("/tables/{}", t.id);
        async move {
            let (_, view) = call(&app, who, "GET", &path, None).await;
            view["encounter"]["combatants"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == "Ogre")
                .unwrap()
                .clone()
        }
    };
    let seen = ogre_of(As::Dm).await;
    assert_eq!(seen["hp"], json!({ "current": 30, "max": 30 }));
    assert_eq!(seen["ac"], 11);
    let id = seen["id"].as_str().unwrap().to_string();
    let theirs = ogre_of(As::Seat(&tam)).await;
    assert_eq!(theirs["health"], "unhurt");
    assert!(theirs.get("hp").is_none() && theirs.get("ac").is_none());

    let at = format!("{fight}/combatants/{id}");
    let refused = call(&app, As::Seat(&tam), "PATCH", &at, Some(json!({ "ac": 5 }))).await;
    assert_eq!(refused.0, StatusCode::FORBIDDEN);
    let hurt = json!({ "hp": { "current": 12, "max": 30 } });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &at, Some(hurt)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(ogre_of(As::Seat(&tam)).await["health"], "bloodied");
    let exact = json!({ "showHealth": "exact" });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &fight, Some(exact)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(ogre_of(As::Seat(&tam)).await["hp"]["current"], 12);
    let no_ac = call(&app, As::Dm, "PATCH", &at, Some(json!({ "ac": null }))).await;
    assert_eq!(no_ac.0, StatusCode::NO_CONTENT);
    assert!(ogre_of(As::Dm).await.get("ac").is_none());

    // One round of poison, and prone until taken off: the poison ends with the Ogre's turn.
    let effects = json!({ "effects": [{ "name": "Poisoned", "rounds": 1 }, { "name": "Prone" }] });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &at, Some(effects)).await.0,
        StatusCode::NO_CONTENT
    );
    let odd = json!({ "effects": [{ "name": "Forever", "rounds": 0 }] });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &at, Some(odd)).await.0,
        StatusCode::BAD_REQUEST
    );
    // The Ogre joined first in line after the turn had begun: Tam, then the Ogre.
    for _ in 0..2 {
        call(&app, As::Dm, "POST", &format!("{fight}/next"), None).await;
    }
    let left = ogre_of(As::Dm).await["effects"].clone();
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert_eq!(left[0]["name"], "Prone");
    let again = json!({ "who": "monsters" });
    let roll = call(&app, As::Dm, "POST", &format!("{fight}/roll"), Some(again)).await;
    assert_eq!(roll.0, StatusCode::NO_CONTENT);
    let (_, view) = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None).await;
    let texts: Vec<String> = view["log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["data"]["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        texts.iter().any(|t| t == "Poisoned ends on Ogre."),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("Initiative rolled again: "))
    );
}
