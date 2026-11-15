//! Spell areas: placed by those the DM lets, changed only by whoever placed them or the DM.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

async fn areas(app: &axum::Router, who: As<'_>, t: &Table) -> Value {
    call(app, who, "GET", &format!("/tables/{}", t.id), None)
        .await
        .1["scene"]["templates"]
        .clone()
}

#[tokio::test]
async fn places_spell_areas_like_the_website() {
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
    let at = format!(
        "/tables/{}/scenes/{}/templates",
        t.id,
        made["id"].as_str().unwrap()
    );
    let fireball = json!({ "shape": "sphere", "x": 350, "y": 350, "size": 20, "color": "#ff9090", "label": "Fireball" });
    let post = |who, body: Value| {
        let (app, path) = (app.clone(), at.clone());
        async move { call(&app, who, "POST", &path, Some(body)).await }
    };

    // Not without the permission.
    assert_eq!(
        post(As::Seat(&tam), fireball.clone()).await.0,
        StatusCode::NOT_FOUND
    );
    for seat in [&tam_seat, &wren_seat] {
        call(
            &app,
            As::Dm,
            "PATCH",
            &format!("/tables/{}/seats/{seat}/permissions", t.id),
            Some(json!({ "permissions": ["move-own", "templates"] })),
        )
        .await;
    }
    let (status, placed) = post(As::Seat(&tam), fireball.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let mine = placed["id"].as_str().unwrap().to_string();
    let mut star = fireball.clone();
    star["shape"] = json!("star");
    assert_eq!(post(As::Seat(&tam), star).await.0, StatusCode::BAD_REQUEST);
    let mut tiny = fireball.clone();
    tiny["size"] = json!(0);
    assert_eq!(post(As::Seat(&tam), tiny).await.0, StatusCode::BAD_REQUEST);
    let mut sneaky = fireball.clone();
    sneaky["hidden"] = json!(true);
    assert_eq!(post(As::Seat(&tam), sneaky).await.0, StatusCode::NOT_FOUND);

    // The one who placed it changes it; another player can't; the DM can. Only what is sent
    // changes.
    let one = format!("{at}/{mine}");
    let patch = |who, body: Value| {
        let (app, path) = (app.clone(), one.clone());
        async move { call(&app, who, "PATCH", &path, Some(body)).await.0 }
    };
    assert_eq!(
        patch(As::Seat(&tam), json!({ "x": 420, "angle": 45 })).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        patch(As::Seat(&wren), json!({ "x": 1 })).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        patch(As::Dm, json!({ "size": 40 })).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        patch(As::Dm, json!({ "x": -5 })).await,
        StatusCode::BAD_REQUEST
    );
    let seen = areas(&app, As::Seat(&tam), &t).await;
    assert_eq!(seen[0]["x"], 420.0);
    assert_eq!(seen[0]["angle"], 45.0);
    assert_eq!(seen[0]["size"], 40);
    assert_eq!(seen[0]["label"], "Fireball");
    assert_eq!(seen[0]["by"], tam_seat.as_str());

    // The DM's hidden area stays off players' screens.
    let trap = json!({ "shape": "cube", "x": 70, "y": 70, "size": 10, "color": "#c9a0f0", "hidden": true });
    let (_, trap) = post(As::Dm, trap).await;
    let trap = trap["id"].as_str().unwrap().to_string();
    assert_eq!(areas(&app, As::Dm, &t).await.as_array().unwrap().len(), 2);
    assert_eq!(
        areas(&app, As::Seat(&tam), &t)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let gone = |who, area: String| {
        let (app, path) = (app.clone(), format!("{at}/{area}"));
        async move { call(&app, who, "DELETE", &path, None).await.0 }
    };
    assert_eq!(gone(As::Seat(&tam), trap).await, StatusCode::NOT_FOUND);
    assert_eq!(
        gone(As::Seat(&wren), mine.clone()).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(gone(As::Seat(&tam), mine).await, StatusCode::NO_CONTENT);
    assert_eq!(
        call(&app, As::Seat(&tam), "DELETE", &at, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, As::Dm, "DELETE", &at, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(areas(&app, As::Dm, &t).await, json!([]));
}
