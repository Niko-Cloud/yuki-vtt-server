//! Fog of war: walls, doors, lights and notes, shared sight, and what players explored.

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
async fn runs_fog_of_war_like_the_website() {
    let app = host();
    let t = table(&app).await;
    let (tam_seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let (_, wren) = join(&app, &t.invite, "Wren", "user-2").await;
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Cave" })),
    )
    .await;
    let at = format!("/tables/{}/scenes/{}", t.id, made["id"].as_str().unwrap());
    let place = call(&app, As::Dm, "POST", &format!("{at}/tokens/party"), None).await;
    assert_eq!(place.0, StatusCode::NO_CONTENT);

    // The party lands on the top row, Tam first: a door one cell over, another far off.
    let lighting = json!({
        "fog": { "mode": "dynamic", "light": "dark", "remember": true },
        "walls": [
            { "id": "near", "a": [140, 0], "b": [140, 70], "door": { "open": false } },
            { "id": "far", "a": [1000, 500], "b": [1000, 600], "door": { "open": false } },
            { "id": "wall", "a": [0, 300], "b": [600, 300] },
        ],
        "lights": [{ "id": "fire", "x": 700, "y": 700, "bright": 10, "dim": 20 }],
    });
    let lit = format!("{at}/lighting");
    let put = |who, body| call(&app, who, "PUT", &lit, Some(body));
    assert_eq!(
        put(As::Seat(&tam), lighting.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        put(As::Dm, lighting.clone()).await.0,
        StatusCode::NO_CONTENT
    );
    let mut off_map = lighting.clone();
    off_map["walls"] = json!([{ "id": "x", "a": [-5, 0], "b": [1, 1] }]);
    assert_eq!(put(As::Dm, off_map).await.0, StatusCode::BAD_REQUEST);
    let mut odd = lighting.clone();
    odd["fog"]["light"] = json!("dusk");
    assert_eq!(put(As::Dm, odd).await.0, StatusCode::BAD_REQUEST);

    // A player opens the door next to their token, not one across the map.
    let door = |who, wall: &str, open: bool| {
        let path = format!("{at}/doors/{wall}");
        let app = app.clone();
        async move {
            call(&app, who, "POST", &path, Some(json!({ "open": open })))
                .await
                .0
        }
    };
    assert_eq!(
        door(As::Seat(&tam), "near", true).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        door(As::Seat(&tam), "far", true).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        door(As::Seat(&tam), "wall", true).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(door(As::Dm, "far", true).await, StatusCode::NO_CONTENT);
    let walls = view(&app, As::Seat(&tam), &t).await["scene"]["walls"].clone();
    assert_eq!(walls[0]["door"]["open"], true);
    assert_eq!(walls[1]["door"]["open"], true);

    // The DM's layer: notes and hidden drawings never reach players.
    let note = call(
        &app,
        As::Dm,
        "POST",
        &format!("{at}/notes"),
        Some(json!({ "x": 100, "y": 100, "text": "Pit trap" })),
    )
    .await;
    assert_eq!(note.0, StatusCode::NO_CONTENT);
    let mine = call(
        &app,
        As::Seat(&tam),
        "POST",
        &format!("{at}/notes"),
        Some(json!({ "x": 1, "y": 1, "text": "Mine" })),
    )
    .await;
    assert_eq!(mine.0, StatusCode::FORBIDDEN);
    let line = json!({ "shape": "rect", "color": "#ff9090", "points": [[0, 0], [70, 70]], "hidden": true });
    let drawn = call(
        &app,
        As::Dm,
        "POST",
        &format!("{at}/drawings"),
        Some(line.clone()),
    )
    .await;
    assert_eq!(drawn.0, StatusCode::NO_CONTENT);
    let dm = view(&app, As::Dm, &t).await;
    assert_eq!(dm["scene"]["notes"][0]["text"], "Pit trap");
    assert_eq!(dm["scene"]["drawings"].as_array().unwrap().len(), 1);
    let player = view(&app, As::Seat(&tam), &t).await;
    assert_eq!(player["scene"]["notes"], json!([]));
    assert_eq!(player["scene"]["drawings"], json!([]));
    // A note the DM lets players read reaches them; the others don't.
    let sign = json!({ "x": 200, "y": 100, "text": "The Rusty Anchor", "shared": true });
    let posted = call(&app, As::Dm, "POST", &format!("{at}/notes"), Some(sign)).await;
    assert_eq!(posted.0, StatusCode::NO_CONTENT);
    let theirs = view(&app, As::Seat(&tam), &t).await["scene"]["notes"].clone();
    assert_eq!(theirs.as_array().unwrap().len(), 1);
    assert_eq!(theirs[0]["text"], "The Rusty Anchor");
    assert!(player["members"][0].get("darkvision").is_some());
    let note_id = dm["scene"]["notes"][0]["id"].as_str().unwrap().to_string();
    let gone = call(
        &app,
        As::Dm,
        "DELETE",
        &format!("{at}/notes/{note_id}"),
        None,
    )
    .await;
    assert_eq!(gone.0, StatusCode::NO_CONTENT);

    // Only the DM sets a token's sight.
    let token = dm["scene"]["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["seatId"] == tam_seat.as_str())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let sight =
        json!({ "vision": { "range": 30, "falloff": 5 }, "light": { "bright": 20, "dim": 40 } });
    let path = format!("{at}/tokens/{token}");
    assert_eq!(
        call(&app, As::Seat(&tam), "PATCH", &path, Some(sight.clone()))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, As::Dm, "PATCH", &path, Some(sight)).await.0,
        StatusCode::NO_CONTENT
    );
    let too_far = json!({ "light": { "bright": 2000, "dim": 40 } });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &path, Some(too_far)).await.0,
        StatusCode::BAD_REQUEST
    );
    call(
        &app,
        As::Dm,
        "PATCH",
        &path,
        Some(json!({ "vision": null })),
    )
    .await;
    let after = view(&app, As::Dm, &t).await["scene"]["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == token.as_str())
        .unwrap()
        .clone();
    assert_eq!(after["light"], json!({ "bright": 20, "dim": 40 }));
    assert!(after.get("vision").is_none());

    // Each player keeps what they explored, until the DM shares sight or forgets it.
    let explored = format!("{at}/explored");
    let add = |who, cells: &str| {
        let (app, path) = (app.clone(), explored.clone());
        let body = json!({ "cells": cells });
        async move { call(&app, who, "POST", &path, Some(body)).await.0 }
    };
    let read = |who| {
        let (app, path) = (app.clone(), explored.clone());
        async move { call(&app, who, "GET", &path, None).await.1["cells"].clone() }
    };
    assert_eq!(
        add(As::Seat(&tam), "0:0-2;1:0").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        add(As::Seat(&tam), "1:1;99:0-3").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(read(As::Seat(&tam)).await, "0:0-2;1:0-1");
    assert_eq!(read(As::Seat(&wren)).await, "");
    let as_tam = call(
        &app,
        As::Dm,
        "GET",
        &format!("{explored}?viewer={tam_seat}"),
        None,
    )
    .await;
    assert_eq!(as_tam.1["cells"], "0:0-2;1:0-1");
    let shared = call(
        &app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/sight", t.id),
        Some(json!({ "shared": true })),
    )
    .await;
    assert_eq!(shared.0, StatusCode::NO_CONTENT);
    assert_eq!(
        view(&app, As::Seat(&tam), &t).await["party"]["sharedSight"],
        true
    );
    assert_eq!(add(As::Seat(&wren), "2:2").await, StatusCode::NO_CONTENT);
    assert_eq!(read(As::Seat(&tam)).await, "2:2");
    let forget = |who| {
        let (app, path) = (app.clone(), explored.clone());
        async move { call(&app, who, "DELETE", &path, None).await.0 }
    };
    assert_eq!(forget(As::Seat(&tam)).await, StatusCode::FORBIDDEN);
    assert_eq!(forget(As::Dm).await, StatusCode::NO_CONTENT);
    assert_eq!(read(As::Seat(&tam)).await, "");
    assert_eq!(
        call(&app, As::Nobody, "GET", &explored, None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn turns_tokens_and_keeps_cones_of_light_and_sight() {
    let app = host();
    let t = table(&app).await;
    let (tam_seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
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
    let tokens = view(&app, As::Dm, &t).await["scene"]["tokens"].clone();
    let id = tokens[0]["id"].as_str().unwrap().to_string();
    assert_eq!(tokens[0]["seatId"], tam_seat.as_str());
    let path = format!("{at}/tokens/{id}");

    // Its player turns it, as they may move it.
    let turned = call(
        &app,
        As::Seat(&tam),
        "PATCH",
        &path,
        Some(json!({ "facing": 90 })),
    )
    .await;
    assert_eq!(turned.0, StatusCode::NO_CONTENT);
    let cones = json!({
        "vision": { "range": 60, "falloff": 10, "shape": "cone", "width": 90 },
        "light": { "bright": 30, "dim": 60, "shape": "cone", "width": 60, "edge": "sharp" },
    });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &path, Some(cones)).await.0,
        StatusCode::NO_CONTENT
    );
    let odd = json!({ "light": { "bright": 30, "dim": 60, "shape": "star" } });
    assert_eq!(
        call(&app, As::Dm, "PATCH", &path, Some(odd)).await.0,
        StatusCode::BAD_REQUEST
    );

    let lantern = json!({ "id": "l", "x": 70, "y": 70, "bright": 15, "dim": 30, "shape": "cone", "width": 45, "angle": 30, "edge": "sharp" });
    let lighting = json!({
        "fog": { "mode": "dynamic", "light": "dark", "remember": true },
        "walls": [],
        "lights": [lantern.clone()],
    });
    let lit = call(
        &app,
        As::Dm,
        "PUT",
        &format!("{at}/lighting"),
        Some(lighting),
    )
    .await;
    assert_eq!(lit.0, StatusCode::NO_CONTENT);
    let mut wide = lantern.clone();
    wide["width"] = json!(1);
    let bad = json!({ "fog": { "mode": "off", "light": "bright", "remember": true }, "walls": [], "lights": [wide] });
    let refused = call(&app, As::Dm, "PUT", &format!("{at}/lighting"), Some(bad)).await;
    assert_eq!(refused.0, StatusCode::BAD_REQUEST);

    let seen = view(&app, As::Seat(&tam), &t).await["scene"].clone();
    assert_eq!(seen["tokens"][0]["facing"], 90.0);
    assert_eq!(seen["tokens"][0]["vision"]["shape"], "cone");
    assert_eq!(seen["tokens"][0]["light"]["edge"], "sharp");
    assert_eq!(seen["lights"][0]["angle"], 30.0);
    assert_eq!(seen["lights"][0]["width"], 45.0);
}

#[tokio::test]
async fn keeps_locked_doors_shut_and_secret_doors_hidden() {
    let app = host();
    let t = table(&app).await;
    let (_, tam) = join(&app, &t.invite, "Tam", "user-1").await;
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
    // Tam lands at the top left, beside all three doors.
    let walls = json!([
        { "id": "locked", "a": [140, 0], "b": [140, 70], "door": { "open": false, "locked": true } },
        { "id": "secret", "a": [0, 140], "b": [70, 140], "door": { "open": false, "secret": true } },
        { "id": "plain", "a": [70, 140], "b": [140, 140], "door": { "open": false } },
        { "id": "glass", "a": [210, 0], "b": [210, 140], "window": true },
    ]);
    let lighting = |walls: Value| json!({ "fog": { "mode": "dynamic", "light": "bright", "remember": true }, "walls": walls, "lights": [] });
    let lit = format!("{at}/lighting");
    let put = call(&app, As::Dm, "PUT", &lit, Some(lighting(walls.clone()))).await;
    assert_eq!(put.0, StatusCode::NO_CONTENT);

    let seen = view(&app, As::Seat(&tam), &t).await["scene"]["walls"].clone();
    assert_eq!(seen[0]["door"], json!({ "open": false, "locked": true }));
    assert_eq!(
        seen[1],
        json!({ "id": "secret", "a": [0.0, 140.0], "b": [70.0, 140.0] })
    );
    assert_eq!(seen[3]["window"], true);

    let door = |who, wall: &str| {
        let path = format!("{at}/doors/{wall}");
        let app = app.clone();
        async move {
            call(&app, who, "POST", &path, Some(json!({ "open": true })))
                .await
                .0
        }
    };
    assert_eq!(
        door(As::Seat(&tam), "locked").await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(door(As::Seat(&tam), "secret").await, StatusCode::NOT_FOUND);
    assert_eq!(door(As::Seat(&tam), "plain").await, StatusCode::NO_CONTENT);
    // Locked means shut for the DM too, until unlocked.
    assert_eq!(door(As::Dm, "locked").await, StatusCode::BAD_REQUEST);
    let mut unlocked = walls.clone();
    unlocked[0]["door"] = json!({ "open": false });
    let put = call(&app, As::Dm, "PUT", &lit, Some(lighting(unlocked))).await;
    assert_eq!(put.0, StatusCode::NO_CONTENT);
    assert_eq!(door(As::Seat(&tam), "locked").await, StatusCode::NO_CONTENT);

    // Opened by the DM, the secret door shows as an open door, its secret kept.
    assert_eq!(door(As::Dm, "secret").await, StatusCode::NO_CONTENT);
    let seen = view(&app, As::Seat(&tam), &t).await["scene"]["walls"].clone();
    assert_eq!(seen[1]["door"], json!({ "open": true }));
    let dm = view(&app, As::Dm, &t).await["scene"]["walls"].clone();
    assert_eq!(dm[1]["door"], json!({ "open": true, "secret": true }));
}

#[tokio::test]
async fn stops_players_tokens_at_walls_unless_the_dm_lets_them_through() {
    let app = host();
    let t = table(&app).await;
    let (tam_seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
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
    let token = view(&app, As::Dm, &t).await["scene"]["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["seatId"] == tam_seat.as_str())
        .unwrap()
        .clone();
    let (x, y) = (token["x"].as_i64().unwrap(), token["y"].as_i64().unwrap());
    let path = format!("{at}/tokens/{}", token["id"].as_str().unwrap());
    let go = |who, to: i64| {
        let (app, path) = (app.clone(), path.clone());
        async move {
            call(&app, who, "PATCH", &path, Some(json!({ "x": to, "y": y })))
                .await
                .0
        }
    };
    let lit = format!("{at}/lighting");
    let put = |walls: Value, collide: Option<bool>| {
        let (app, lit) = (app.clone(), lit.clone());
        let mut fog = json!({ "mode": "off", "light": "bright", "remember": true });
        if let Some(collide) = collide {
            fog["collide"] = json!(collide);
        }
        async move {
            let body = json!({ "fog": fog, "walls": walls, "lights": [] });
            call(&app, As::Dm, "PUT", &lit, Some(body)).await.0
        }
    };
    // A wall on the far side of the next cell over, from top to bottom.
    let edge = (x + 2) * 70;
    let wall = json!({ "id": "w", "a": [edge, 0], "b": [edge, 700] });
    assert_eq!(put(json!([wall]), None).await, StatusCode::NO_CONTENT);
    assert_eq!(go(As::Seat(&tam), x + 1).await, StatusCode::NO_CONTENT);
    assert_eq!(go(As::Seat(&tam), x + 3).await, StatusCode::BAD_REQUEST);
    // An open door lets them through; the DM goes anywhere.
    let mut door = wall.clone();
    door["door"] = json!({ "open": true });
    assert_eq!(put(json!([door]), None).await, StatusCode::NO_CONTENT);
    assert_eq!(go(As::Seat(&tam), x + 3).await, StatusCode::NO_CONTENT);
    door["door"] = json!({ "open": false });
    assert_eq!(put(json!([door]), None).await, StatusCode::NO_CONTENT);
    assert_eq!(go(As::Seat(&tam), x + 1).await, StatusCode::BAD_REQUEST);
    assert_eq!(go(As::Dm, x + 1).await, StatusCode::NO_CONTENT);
    // With walls not stopping tokens, players pass too.
    assert_eq!(
        put(json!([wall]), Some(false)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(go(As::Seat(&tam), x + 3).await, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn hides_monsters_under_painted_fog_from_players() {
    let app = host();
    let t = table(&app).await;
    let (_, tam) = join(&app, &t.invite, "Tam", "user-1").await;
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
    for (name, x) in [("Rat", 1), ("Ogre", 10)] {
        let token = json!({ "name": name, "x": x, "y": x });
        call(&app, As::Dm, "POST", &format!("{at}/tokens"), Some(token)).await;
    }
    // The whole map covered, the top left corner revealed.
    let lighting = json!({
        "fog": { "mode": "off", "light": "bright", "remember": true, "cover": true },
        "walls": [],
        "lights": [],
        "fogAreas": [{ "id": "f1", "reveal": true, "points": [[0, 0], [280, 0], [280, 280], [0, 280]] }],
    });
    let lit = format!("{at}/lighting");
    let put = call(&app, As::Dm, "PUT", &lit, Some(lighting.clone())).await;
    assert_eq!(put.0, StatusCode::NO_CONTENT);
    let names = |who| {
        let app = app.clone();
        let t = &t;
        async move {
            view(&app, who, t).await["scene"]["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    let theirs = names(As::Seat(&tam)).await;
    assert!(theirs.contains(&"Rat".to_string()) && theirs.contains(&"Tam".to_string()));
    assert!(!theirs.contains(&"Ogre".to_string()));
    assert!(names(As::Dm).await.contains(&"Ogre".to_string()));
    // Lighting sent without the painted areas keeps them; odd areas are refused.
    let mut without = lighting.clone();
    without.as_object_mut().unwrap().remove("fogAreas");
    assert_eq!(
        call(&app, As::Dm, "PUT", &lit, Some(without)).await.0,
        StatusCode::NO_CONTENT
    );
    let kept = view(&app, As::Dm, &t).await["scene"]["fogAreas"].clone();
    assert_eq!(kept.as_array().unwrap().len(), 1);
    let mut odd = lighting;
    odd["fogAreas"] = json!([{ "id": "f2", "points": [[0, 0], [10, 10]] }]);
    assert_eq!(
        call(&app, As::Dm, "PUT", &lit, Some(odd)).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn keeps_a_fog_colour_like_the_website() {
    let app = host();
    let t = table(&app).await;
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Cave" })),
    )
    .await;
    let at = format!("/tables/{}/scenes/{}", t.id, made["id"].as_str().unwrap());
    let lit = format!("{at}/lighting");
    let base = json!({
        "fog": { "mode": "off", "light": "bright", "remember": true },
        "walls": [],
        "lights": [],
    });
    let mut coloured = base.clone();
    coloured["fog"]["color"] = json!("#203040");
    assert_eq!(
        call(&app, As::Dm, "PUT", &lit, Some(coloured)).await.0,
        StatusCode::NO_CONTENT
    );
    let kept = view(&app, As::Dm, &t).await["scene"]["fog"]["color"].clone();
    assert_eq!(kept, json!("#203040"));
    // A colour that is not #rrggbb is refused, as on the website.
    for bad in ["red", "#12345", "#1234567", "203040"] {
        let mut odd = base.clone();
        odd["fog"]["color"] = json!(bad);
        assert_eq!(
            call(&app, As::Dm, "PUT", &lit, Some(odd)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    // No colour means the default dark: the field is simply absent.
    assert_eq!(
        call(&app, As::Dm, "PUT", &lit, Some(base)).await.0,
        StatusCode::NO_CONTENT
    );
    let plain = view(&app, As::Dm, &t).await["scene"]["fog"].clone();
    assert!(plain.get("color").is_none());
}

#[tokio::test]
async fn keeps_circle_and_brush_fog_shapes_like_the_website() {
    let app = host();
    let t = table(&app).await;
    let (_, tam) = join(&app, &t.invite, "Tam", "user-1").await;
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
    // Rat stands in the circle, Bat in the brush band, Ogre outside both.
    for (name, x, y) in [("Rat", 4, 3), ("Bat", 2, 1), ("Ogre", 10, 10)] {
        let token = json!({ "name": name, "x": x, "y": y });
        call(&app, As::Dm, "POST", &format!("{at}/tokens"), Some(token)).await;
    }
    // A circle is a 48-corner polygon; a brush stroke a band with round ends.
    let circle: Vec<Value> = (0..48)
        .map(|i| {
            let a = (i as f64 / 48.0) * std::f64::consts::TAU;
            json!([
                (350.0 + a.cos() * 100.0).round(),
                (245.0 + a.sin() * 100.0).round()
            ])
        })
        .collect();
    let band = json!([
        [100, 80],
        [300, 80],
        [320, 100],
        [300, 120],
        [100, 120],
        [80, 100]
    ]);
    let lighting = json!({
        "fog": { "mode": "off", "light": "bright", "remember": true },
        "walls": [],
        "lights": [],
        "fogAreas": [
            { "id": "circle", "points": circle },
            { "id": "brush", "points": band },
        ],
    });
    let lit = format!("{at}/lighting");
    assert_eq!(
        call(&app, As::Dm, "PUT", &lit, Some(lighting)).await.0,
        StatusCode::NO_CONTENT
    );
    let kept = view(&app, As::Dm, &t).await["scene"]["fogAreas"].clone();
    let areas = kept.as_array().unwrap();
    assert_eq!(areas.len(), 2);
    assert_eq!(areas[0]["points"].as_array().unwrap().len(), 48);
    assert_eq!(areas[1]["points"].as_array().unwrap().len(), 6);
    let names = |who| {
        let app = app.clone();
        let t = &t;
        async move {
            view(&app, who, t).await["scene"]["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    let theirs = names(As::Seat(&tam)).await;
    assert!(theirs.contains(&"Tam".to_string()) && theirs.contains(&"Ogre".to_string()));
    assert!(!theirs.contains(&"Rat".to_string()) && !theirs.contains(&"Bat".to_string()));
    let dms = names(As::Dm).await;
    assert!(dms.contains(&"Rat".to_string()) && dms.contains(&"Bat".to_string()));
}

#[tokio::test]
async fn keeps_a_fog_layer_mode_like_the_website() {
    let app = host();
    let t = table(&app).await;
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(json!({ "name": "Cave" })),
    )
    .await;
    let at = format!("/tables/{}/scenes/{}", t.id, made["id"].as_str().unwrap());
    let lit = format!("{at}/lighting");
    let base = json!({
        "fog": { "mode": "off", "light": "bright", "remember": true },
        "walls": [],
        "lights": [],
    });
    for layer in ["single", "stacked"] {
        let mut set = base.clone();
        set["fog"]["layer"] = json!(layer);
        assert_eq!(
            call(&app, As::Dm, "PUT", &lit, Some(set)).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            view(&app, As::Dm, &t).await["scene"]["fog"]["layer"],
            json!(layer)
        );
    }
    // A mode the map does not know is refused; none means stacked, the field absent.
    let mut odd = base.clone();
    odd["fog"]["layer"] = json!("many");
    assert_eq!(
        call(&app, As::Dm, "PUT", &lit, Some(odd)).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, As::Dm, "PUT", &lit, Some(base)).await.0,
        StatusCode::NO_CONTENT
    );
    let plain = view(&app, As::Dm, &t).await["scene"]["fog"].clone();
    assert!(plain.get("layer").is_none());
}

#[tokio::test]
async fn keeps_trimmed_and_joined_fog_shapes_like_the_website() {
    let app = host();
    let t = table(&app).await;
    let (_, tam) = join(&app, &t.invite, "Tam", "user-1").await;
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
    for (name, x, y) in [("Rat", 0, 1), ("Ogre", 10, 10)] {
        let token = json!({ "name": name, "x": x, "y": y });
        call(&app, As::Dm, "POST", &format!("{at}/tokens"), Some(token)).await;
    }
    // A join can leave an L-shaped outline; a trim a smaller piece. Both are plain polygons.
    let joined = json!([
        [0, 0],
        [200, 0],
        [200, 100],
        [100, 100],
        [100, 200],
        [0, 200]
    ]);
    let trimmed = json!([[300, 300], [400, 300], [400, 400], [300, 400]]);
    let lighting = json!({
        "fog": { "mode": "off", "light": "bright", "remember": true },
        "walls": [],
        "lights": [],
        "fogAreas": [
            { "id": "joined", "points": joined },
            { "id": "trimmed", "points": trimmed },
        ],
    });
    let lit = format!("{at}/lighting");
    assert_eq!(
        call(&app, As::Dm, "PUT", &lit, Some(lighting)).await.0,
        StatusCode::NO_CONTENT
    );
    let kept = view(&app, As::Dm, &t).await["scene"]["fogAreas"].clone();
    let areas = kept.as_array().unwrap();
    assert_eq!(areas.len(), 2);
    assert_eq!(areas[0]["points"].as_array().unwrap().len(), 6);
    assert_eq!(areas[1]["points"].as_array().unwrap().len(), 4);
    let names = |who| {
        let app = app.clone();
        let t = &t;
        async move {
            view(&app, who, t).await["scene"]["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    // Rat stands in the L (35, 105); Ogre far outside both shapes.
    let theirs = names(As::Seat(&tam)).await;
    assert!(theirs.contains(&"Tam".to_string()) && theirs.contains(&"Ogre".to_string()));
    assert!(!theirs.contains(&"Rat".to_string()));
    assert!(names(As::Dm).await.contains(&"Rat".to_string()));
}
