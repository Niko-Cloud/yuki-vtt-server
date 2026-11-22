//! Stairs and portals, as the website has them: through on one map, or to another map that
//! the token's player then sees.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

#[tokio::test]
async fn takes_a_token_through_a_passage_on_its_map_or_to_another_its_player_sees() {
    let (app, state) = host_and_state();
    let t = table(&app).await;
    let (tam_seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    // Kept for the whole test, so the closures below can take Tam's seat as who calls.
    let tam: &'static str = Box::leak(tam.into_boxed_str());
    let player = As::Seat(tam);
    let (wren_seat, _) = join(&app, &t.invite, "Wren", "user-2").await;
    let make = |name: &'static str| {
        let (app, path) = (app.clone(), format!("/tables/{}/scenes", t.id));
        async move {
            call(&app, As::Dm, "POST", &path, Some(json!({ "name": name })))
                .await
                .1["id"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let cave = make("Cave").await;
    let attic = make("Attic").await;
    let at = |scene: &str| format!("/tables/{}/scenes/{scene}", t.id);
    call(&app, As::Dm, "POST", &format!("{}/show", at(&cave)), None).await;
    call(
        &app,
        As::Dm,
        "POST",
        &format!("{}/tokens/party", at(&cave)),
        None,
    )
    .await;
    let scene_of = |who: As<'static>, scene: Option<String>| {
        let (app, path) = (
            app.clone(),
            match scene {
                Some(s) => format!("/tables/{}?scene={s}", t.id),
                None => format!("/tables/{}", t.id),
            },
        );
        async move { call(&app, who, "GET", &path, None).await.1["scene"].clone() }
    };
    let token_of = |scene: &Value, seat: &str| {
        scene["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["seatId"] == seat)
            .cloned()
    };
    let shown = scene_of(As::Dm, Some(cave.clone())).await;
    let tam_token = token_of(&shown, &tam_seat).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let wren_token = token_of(&shown, &wren_seat).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    for (token, x) in [(&tam_token, 1), (&wren_token, 3)] {
        call(
            &app,
            As::Dm,
            "PATCH",
            &format!("{}/tokens/{token}", at(&cave)),
            Some(json!({ "x": x, "y": 1 })),
        )
        .await;
    }
    let place = |scene: String, x: u32, y: u32, name: &'static str| {
        let (app, path) = (app.clone(), format!("{}/passages", at(&scene)));
        async move {
            let (status, made) = call(
                &app,
                As::Dm,
                "POST",
                &path,
                Some(json!({ "x": x, "y": y, "name": name })),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
            made["id"].as_str().unwrap().to_string()
        }
    };
    let stairs = place(cave.clone(), 105, 105, "Stairs up").await;
    let landing = place(attic.clone(), 455, 315, "Stairs down").await;
    let trapdoor = place(cave.clone(), 245, 105, "Trapdoor").await;
    let corner = place(cave.clone(), 595, 385, "Chute end").await;
    let (refused, _) = call(
        &app,
        player,
        "POST",
        &format!("{}/passages", at(&cave)),
        Some(json!({ "x": 1, "y": 1 })),
    )
    .await;
    assert_ne!(refused, StatusCode::CREATED);
    let link = |scene: String, passage: String, body: Value| {
        let (app, path) = (app.clone(), format!("{}/passages/{passage}", at(&scene)));
        async move { call(&app, As::Dm, "PATCH", &path, Some(body)).await.0 }
    };
    assert_eq!(
        link(
            cave.clone(),
            stairs.clone(),
            json!({ "to": { "map": attic, "passage": landing } })
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        link(
            cave.clone(),
            trapdoor.clone(),
            json!({ "to": { "map": cave, "passage": corner }, "oneWay": true, "ask": false, "hidden": true })
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let passages_on = |scene: String| {
        let scene_at = scene_of(As::Dm, Some(scene));
        async move { scene_at.await["passages"].clone() }
    };
    // Two-way: the landing leads back; the chute end doesn't.
    assert_eq!(
        passages_on(attic.clone()).await[0]["to"],
        json!({ "map": cave, "passage": stairs })
    );
    let cave_passages = passages_on(cave.clone()).await;
    let find = |list: &Value, id: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .cloned()
            .unwrap()
    };
    assert!(find(&cave_passages, &corner).get("to").is_none());
    assert_eq!(find(&cave_passages, &trapdoor)["ask"], false);
    // Players don't see the hidden trapdoor.
    let seen = scene_of(player, None).await;
    assert_eq!(seen["passages"].as_array().unwrap().len(), 2);
    let through = |who: As<'static>, scene: String, token: String, passage: String| {
        let (app, path) = (
            app.clone(),
            format!("{}/tokens/{token}/through", at(&scene)),
        );
        async move {
            call(
                &app,
                who,
                "POST",
                &path,
                Some(json!({ "passage": passage })),
            )
            .await
            .0
        }
    };
    // Not standing on it, not going.
    assert_eq!(
        through(player, cave.clone(), tam_token.clone(), trapdoor.clone()).await,
        StatusCode::BAD_REQUEST
    );
    // The DM sends Wren down the trapdoor, across the cave.
    assert_eq!(
        through(As::Dm, cave.clone(), wren_token.clone(), trapdoor.clone()).await,
        StatusCode::NO_CONTENT
    );
    let wren_at = |scene: Value| {
        let w = token_of(&scene, &wren_seat).unwrap();
        (w["x"].as_f64(), w["y"].as_f64())
    };
    assert_eq!(
        wren_at(scene_of(As::Dm, Some(cave.clone())).await),
        (Some(8.0), Some(5.0))
    );
    // It goes at once: a token that stops on it falls through by itself.
    call(
        &app,
        As::Dm,
        "PATCH",
        &format!("{}/tokens/{wren_token}", at(&cave)),
        Some(json!({ "x": 3, "y": 1 })),
    )
    .await;
    assert_eq!(
        wren_at(scene_of(As::Dm, Some(cave.clone())).await),
        (Some(8.0), Some(5.0))
    );
    // Locked, only the DM sends tokens through.
    link(cave.clone(), stairs.clone(), json!({ "locked": true })).await;
    assert_eq!(
        through(player, cave.clone(), tam_token.clone(), stairs.clone()).await,
        StatusCode::BAD_REQUEST
    );
    link(cave.clone(), stairs.clone(), json!({ "locked": false })).await;
    // Tam goes up: off the cave, onto the attic by the landing, and Tam's player sees the attic.
    assert_eq!(
        through(player, cave.clone(), tam_token.clone(), stairs.clone()).await,
        StatusCode::NO_CONTENT
    );
    assert!(token_of(&scene_of(As::Dm, Some(cave.clone())).await, &tam_seat).is_none());
    let up = token_of(&scene_of(As::Dm, Some(attic.clone())).await, &tam_seat).unwrap();
    assert_eq!((up["x"].as_f64(), up["y"].as_f64()), (Some(6.0), Some(4.0)));
    assert_eq!(scene_of(player, None).await["id"], attic.as_str());
    // A drag up there goes live to those on the attic only, not to Wren in the cave.
    let mut events = state.events.listen(&t.id);
    let drag =
        json!({ "kind": "drag", "points": [[455, 315]], "tab": "tab-1", "token": tam_token });
    let (status, _) = call(
        &app,
        player,
        "POST",
        &format!("/tables/{}/mark", t.id),
        Some(drag),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent = events.recv().await.unwrap();
    let to = |seat: &str| yuki_vtt_server::events::for_me(sent.clone(), false, &[seat.to_string()]);
    assert!(
        to(&tam_seat).unwrap().contains("\"sceneId\":\"")
            && to(&tam_seat).unwrap().contains(&attic)
    );
    assert_eq!(to(&wren_seat), None);
    // The cave is out of Tam's reach now.
    let (status, _) = call(
        &app,
        player,
        "PATCH",
        &format!("{}/tokens/{wren_token}", at(&cave)),
        Some(json!({ "x": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The DM sees where Tam went.
    let away = |app: axum::Router, path: String| async move {
        call(&app, As::Dm, "GET", &path, None).await.1["away"].clone()
    };
    let table_at = format!("/tables/{}", t.id);
    assert_eq!(
        away(app.clone(), table_at.clone()).await,
        json!({ tam_seat.clone(): attic })
    );
    // Showing a map brings everyone back to it, Tam's token too: by the stairs that led there.
    call(&app, As::Dm, "POST", &format!("{}/show", at(&cave)), None).await;
    assert_eq!(scene_of(player, None).await["id"], cave.as_str());
    assert!(token_of(&scene_of(As::Dm, Some(attic.clone())).await, &tam_seat).is_none());
    let back = token_of(&scene_of(As::Dm, Some(cave.clone())).await, &tam_seat).unwrap();
    assert_eq!(
        (back["x"].as_f64(), back["y"].as_f64()),
        (Some(1.0), Some(1.0))
    );
    assert_eq!(away(app.clone(), table_at).await, json!({}));
    // The DM sends the whole party up: each by the landing, and Tam's player sees the attic.
    let party = format!("{}/passages/{stairs}/party", at(&cave));
    let (refused, _) = call(&app, player, "POST", &party, None).await;
    assert_ne!(refused, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, As::Dm, "POST", &party, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let attic_now = scene_of(As::Dm, Some(attic.clone())).await;
    let mut spots: Vec<(f64, f64)> = attic_now["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["seatId"].is_string())
        .map(|t| (t["x"].as_f64().unwrap(), t["y"].as_f64().unwrap()))
        .collect();
    spots.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(spots, vec![(6.0, 4.0), (7.0, 4.0)]);
    assert_eq!(scene_of(player, None).await["id"], attic.as_str());
    // The log tells who went where; the hidden trapdoor's trips only the DM reads.
    let texts = |who: As<'static>| {
        let (app, path) = (app.clone(), format!("/tables/{}", t.id));
        async move {
            call(&app, who, "GET", &path, None).await.1["log"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|e| e["data"]["text"].as_str().map(String::from))
                .collect::<Vec<_>>()
        }
    };
    let dm_reads = texts(As::Dm).await;
    for line in [
        "Wren went through Trapdoor.",
        "Tam went through Stairs up to Attic.",
        "The party went through Stairs up to Attic.",
    ] {
        assert!(dm_reads.iter().any(|t| t == line), "{line} in {dm_reads:?}");
    }
    assert!(
        !texts(player)
            .await
            .iter()
            .any(|t| t == "Wren went through Trapdoor.")
    );
    call(&app, As::Dm, "POST", &format!("{}/show", at(&cave)), None).await;
    // The attic goes: the stairs lead nowhere.
    call(&app, As::Dm, "DELETE", &at(&attic), None).await;
    assert!(
        find(&passages_on(cave.clone()).await, &stairs)
            .get("to")
            .is_none()
    );
}
