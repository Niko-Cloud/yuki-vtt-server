//! What a player may do on the map, one permission at a time, as the website has it.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

async fn view(app: &axum::Router, who: As<'_>, t: &Table) -> Value {
    call(app, who, "GET", &format!("/tables/{}", t.id), None)
        .await
        .1
}

#[tokio::test]
async fn lets_a_player_do_each_thing_by_its_own_permission() {
    let (app, state) = host_and_state();
    let t = table(&app).await;
    let (seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Cave" })),
    )
    .await;
    let at = format!("/tables/{}/scenes/{}", t.id, made["id"].as_str().unwrap());
    call(&app, As::Dm, "POST", &format!("{at}/tokens/party"), None).await;
    call(
        &app,
        As::Dm,
        "POST",
        &format!("{at}/tokens"),
        Some(json!({ "name": "Ogre", "x": 6, "y": 3 })),
    )
    .await;
    let tokens = view(&app, As::Dm, &t).await["scene"]["tokens"].clone();
    let id_of = |name: &str, tokens: &Value| {
        tokens
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (mine, ogre) = (id_of("Tam", &tokens), id_of("Ogre", &tokens));
    let allow = |permissions: Value| {
        let (app, path) = (
            app.clone(),
            format!("/tables/{}/seats/{seat}/permissions", t.id),
        );
        async move {
            call(
                &app,
                As::Dm,
                "PATCH",
                &path,
                Some(json!({ "permissions": permissions })),
            )
            .await
            .0
        }
    };
    let change = |token: String, body: Value| {
        let (app, path, tam) = (app.clone(), format!("{at}/tokens/{token}"), tam.clone());
        async move {
            call(&app, As::Seat(&tam), "PATCH", &path, Some(body))
                .await
                .0
        }
    };
    // Changing a token's look goes for their own only, and doesn't take in its size.
    assert_eq!(
        allow(json!(["move-own", "token-edit"])).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        change(mine.clone(), json!({ "name": "Sir Tam" })).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        change(mine.clone(), json!({ "size": 2 })).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        change(ogre.clone(), json!({ "name": "Bob" })).await,
        StatusCode::NOT_FOUND
    );
    // A token a player adds is theirs: to move, and to take off again.
    assert_eq!(
        allow(json!(["move-own", "token-add", "token-remove"])).await,
        StatusCode::NO_CONTENT
    );
    call(
        &app,
        As::Seat(&tam),
        "POST",
        &format!("{at}/tokens"),
        Some(json!({ "name": "Bat", "x": 1, "y": 1 })),
    )
    .await;
    let tokens = view(&app, As::Dm, &t).await["scene"]["tokens"].clone();
    let bat = id_of("Bat", &tokens);
    let bats = tokens
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Bat")
        .unwrap()
        .clone();
    assert_eq!(bats["movedBy"], json!([seat]));
    assert_eq!(
        change(bat.clone(), json!({ "x": 2 })).await,
        StatusCode::NO_CONTENT
    );
    let remove = |token: String| {
        let (app, path, tam) = (app.clone(), format!("{at}/tokens/{token}"), tam.clone());
        async move { call(&app, As::Seat(&tam), "DELETE", &path, None).await.0 }
    };
    assert_eq!(remove(ogre.clone()).await, StatusCode::NOT_FOUND);
    assert_eq!(remove(bat).await, StatusCode::NO_CONTENT);
    // Any token, with the permission that says so.
    assert_eq!(
        allow(json!(["move-own", "token-resize", "token-any"])).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        change(ogre.clone(), json!({ "size": 2 })).await,
        StatusCode::NO_CONTENT
    );
    // The old broader names are no longer taken, but a seat that has them keeps what they
    // allowed.
    assert_eq!(allow(json!(["tokens"])).await, StatusCode::BAD_REQUEST);
    state
        .db()
        .execute(
            "UPDATE seats SET permissions = '[\"tokens\",\"draw\"]' WHERE id = ?1",
            [&seat],
        )
        .unwrap();
    assert_eq!(
        view(&app, As::Seat(&tam), &t).await["you"]["permissions"],
        json!([
            "token-add",
            "token-edit",
            "token-resize",
            "token-arrange",
            "token-remove",
            "token-any",
            "prop-add",
            "prop-edit",
            "prop-remove",
            "upload",
            "drawings",
            "ping"
        ])
    );
    assert_eq!(
        change(ogre, json!({ "name": "Ogre" })).await,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn lets_a_player_pin_notes_everyone_reads_and_take_off_their_own() {
    let app = host();
    let t = table(&app).await;
    let (tam_seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let (wren_seat, wren) = join(&app, &t.invite, "Wren", "user-2").await;
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Cave" })),
    )
    .await;
    let notes = format!(
        "/tables/{}/scenes/{}/notes",
        t.id,
        made["id"].as_str().unwrap()
    );
    let pin = json!({ "x": 70, "y": 70, "text": "Loose stone here", "shared": false });
    let (refused, _) = call(&app, As::Seat(&tam), "POST", &notes, Some(pin.clone())).await;
    assert_eq!(refused, StatusCode::NOT_FOUND);
    for seat in [&tam_seat, &wren_seat] {
        call(
            &app,
            As::Dm,
            "PATCH",
            &format!("/tables/{}/seats/{seat}/permissions", t.id),
            Some(json!({ "permissions": ["move-own", "notes"] })),
        )
        .await;
    }
    let (pinned, _) = call(&app, As::Seat(&tam), "POST", &notes, Some(pin)).await;
    assert_eq!(pinned, StatusCode::NO_CONTENT);
    call(
        &app,
        As::Dm,
        "POST",
        &notes,
        Some(json!({ "x": 1, "y": 1, "text": "Trap" })),
    )
    .await;
    let stored = view(&app, As::Dm, &t).await["scene"]["notes"].clone();
    let find = |text: &str| {
        stored
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["text"] == text)
            .unwrap()
            .clone()
    };
    let (theirs, trap) = (find("Loose stone here"), find("Trap"));
    // A player's note is for everyone, and says whose it is.
    assert_eq!(theirs["shared"], true);
    assert_eq!(theirs["by"], tam_seat.as_str());
    assert_eq!(
        view(&app, As::Seat(&tam), &t).await["scene"]["notes"],
        json!([theirs])
    );
    let take_off = |who: &str, note: &Value| {
        let path = format!("{notes}/{}", note["id"].as_str().unwrap());
        let (app, who) = (app.clone(), who.to_string());
        async move { call(&app, As::Seat(&who), "DELETE", &path, None).await.0 }
    };
    assert_eq!(take_off(&tam, &trap).await, StatusCode::NOT_FOUND);
    assert_eq!(take_off(&wren, &theirs).await, StatusCode::NOT_FOUND);
    assert_eq!(take_off(&tam, &theirs).await, StatusCode::NO_CONTENT);
}
