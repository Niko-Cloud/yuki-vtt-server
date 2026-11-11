//! Whispers between the DM and one player, and the DM hearing what a player changed.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

// A character as the website hands it over, with its numbers and gear as facts.
fn with_facts(name: &str, player: &str, facts: Value, signed_for: For) -> Value {
    let mut snapshot = json!({
        "playerId": player,
        "issuedAt": issued_now(),
        "summary": { "name": name, "classes": "Fighter 3", "armorClass": 15, "initiative": 2, "maxHitPoints": 28, "edition": "2014" },
        "character": { "name": name, "state": { "currentHitPoints": null, "temporaryHitPoints": 0, "conditions": [] } },
        "facts": facts,
    });
    match signed_for {
        For::Join(table) => snapshot["tableId"] = json!(table),
        For::Seat(seat) => snapshot["seatId"] = json!(seat),
    }
    sign(&snapshot.to_string())
}

fn facts(hp: &str, ac: &str, potions: Option<u32>) -> Value {
    let mut list = vec![
        json!({ "key": "hp", "label": "HP", "value": hp }),
        json!({ "key": "ac", "label": "AC", "value": ac }),
        json!({ "key": "conditions", "label": "Conditions", "value": "none" }),
    ];
    if let Some(n) = potions {
        list.push(json!({ "key": "item:potion", "label": "Potion", "value": n.to_string(), "kind": "item" }));
    }
    Value::Array(list)
}

fn of_kind<'a>(log: &'a Value, kind: &str) -> Vec<&'a Value> {
    log.as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == kind)
        .collect()
}

#[tokio::test]
async fn whispers_reach_only_the_dm_and_one_player() {
    let app = host();
    let t = table(&app).await;
    let (tam_seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let (_, wren) = join(&app, &t.invite, "Wren", "user-2").await;
    let say = |to: &str, text: &str| json!({ "kind": "chat", "text": text, "to": to });
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/log", t.id),
        Some(say(&tam_seat, "You spot a hidden door.")),
    )
    .await;
    call(
        &app,
        As::Seat(&wren),
        "POST",
        &format!("/tables/{}/log", t.id),
        Some(say("dm", "I pocket the gem.")),
    )
    .await;

    let texts = |view: &Value| {
        of_kind(&view["log"], "chat")
            .iter()
            .map(|e| e["data"]["text"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let (_, dm) = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None).await;
    assert_eq!(texts(&dm), ["You spot a hidden door.", "I pocket the gem."]);
    assert_eq!(dm["log"][0]["data"]["to"], "Tam");
    assert_eq!(dm["log"][1]["data"]["to"], "the DM");
    let (_, as_tam) = call(
        &app,
        As::Seat(&tam),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(texts(&as_tam), ["You spot a hidden door."]);
    let (_, as_wren) = call(
        &app,
        As::Seat(&wren),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(texts(&as_wren), ["I pocket the gem."]);

    let (status, _) = call(
        &app,
        As::Seat(&wren),
        "POST",
        &format!("/tables/{}/log", t.id),
        Some(say(&tam_seat, "psst")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn tells_the_dm_what_a_player_changed_but_not_the_dms_own_damage() {
    let app = host();
    let t = table(&app).await;
    let (status, seat) = call(
        &app,
        As::Nobody,
        "POST",
        &format!("/join/{}", t.invite),
        Some(with_facts(
            "Tam",
            "user-1",
            facts("28/28", "15", None),
            For::Join(&t.id),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (seat_id, token) = (
        seat["seatId"].as_str().unwrap(),
        seat["seatToken"].as_str().unwrap(),
    );

    // The DM hits Tam for 6: the player's next save says 22/28 too, which isn't their change.
    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/seats/{seat_id}/change", t.id),
        Some(json!({ "kind": "damage", "amount": 6 })),
    )
    .await;
    let push = |facts: Value| with_facts("Tam", "user-1", facts, For::Seat(seat_id));
    call(
        &app,
        As::Nobody,
        "PUT",
        &format!("/seats/{token}"),
        Some(push(facts("22/28", "17", Some(2)))),
    )
    .await;
    call(
        &app,
        As::Nobody,
        "PUT",
        &format!("/seats/{token}"),
        Some(push(facts("22/28", "17", Some(3)))),
    )
    .await;

    let (_, dm) = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None).await;
    let notes = of_kind(&dm["log"], "change");
    // Two saves close together, one note: AC and the potions, not the DM's damage.
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["author"], "Tam");
    assert_eq!(notes[0]["hidden"], true);
    let changed: Vec<(String, Value, Value)> = notes[0]["data"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["key"].as_str().unwrap().to_string(),
                c["from"].clone(),
                c["to"].clone(),
            )
        })
        .collect();
    assert_eq!(
        changed,
        [
            ("ac".into(), json!("15"), json!("17")),
            ("item:potion".into(), Value::Null, json!("3"))
        ]
    );

    let (_, player) = call(
        &app,
        As::Seat(token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert!(of_kind(&player["log"], "change").is_empty());
}
