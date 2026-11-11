//! Asking to join: the DM lets newcomers in, or turns them away.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;

// A table as a new one is: newcomers wait for the DM.
async fn asking(app: &axum::Router) -> Table {
    let t = table(app).await;
    call(
        app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/ask", t.id),
        Some(json!({ "askToJoin": true })),
    )
    .await;
    t
}

async fn ask(app: &axum::Router, invite: &str, name: &str, player: &str) -> (String, String) {
    let (status, asked) = call(
        app,
        As::Nobody,
        "POST",
        &format!("/join/{invite}"),
        Some(signed(name, player, 15, fresh())),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{asked}");
    assert_eq!(asked["waiting"], true);
    (
        asked["seatId"].as_str().unwrap().into(),
        asked["seatToken"].as_str().unwrap().into(),
    )
}

#[tokio::test]
async fn keeps_newcomers_waiting_until_the_dm_lets_them_in() {
    let app = host();
    let t = asking(&app).await;
    let (request, token) = ask(&app, &t.invite, "Wren", "user-2").await;

    // Waiting: not at the table yet.
    let (_, status) = call(
        &app,
        As::Nobody,
        "GET",
        &format!("/seats/{token}/status"),
        None,
    )
    .await;
    assert_eq!(status["status"], "waiting");
    let (status, _) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The DM sees who it is, in the DM view and the map room.
    let (_, party) = call(
        &app,
        As::Dm,
        "GET",
        &format!("/tables/{}/party", t.id),
        None,
    )
    .await;
    assert_eq!(party["askToJoin"], true);
    assert_eq!(party["requests"][0]["name"], "Wren");
    assert_eq!(party["requests"][0]["classes"], "Fighter 3");
    assert_eq!(party["members"], json!([]));
    let (_, view) = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None).await;
    assert_eq!(view["requests"][0]["id"], request.as_str());

    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/requests/{request}/accept", t.id),
        None,
    )
    .await;
    let (_, status) = call(
        &app,
        As::Nobody,
        "GET",
        &format!("/seats/{token}/status"),
        None,
    )
    .await;
    assert_eq!(status["status"], "seated");
    // The same token is the seat's now.
    let (status, view) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["you"]["seatIds"], json!([request]));
    assert_eq!(view["requests"], json!([]));
}

#[tokio::test]
async fn turns_people_away_and_only_the_dm_decides() {
    let app = host();
    let t = asking(&app).await;
    let (seat, member) = join_seated(&app, &t).await;
    let (request, token) = ask(&app, &t.invite, "Stranger", "user-9").await;

    // A player at the table can't let anyone in, and doesn't see who waits.
    let (status, _) = call(
        &app,
        As::Seat(&member),
        "POST",
        &format!("/tables/{}/requests/{request}/accept", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, view) = call(
        &app,
        As::Seat(&member),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    assert_eq!(view["requests"], json!([]));

    call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/requests/{request}/reject", t.id),
        None,
    )
    .await;
    let (status, refused) = call(
        &app,
        As::Nobody,
        "GET",
        &format!("/seats/{token}/status"),
        None,
    )
    .await;
    assert_eq!(
        (status, refused["message"].as_str()),
        (StatusCode::NOT_FOUND, Some("The DM didn't let you in"))
    );
    let (status, _) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/requests/{request}/accept", t.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, party) = call(
        &app,
        As::Dm,
        "GET",
        &format!("/tables/{}/party", t.id),
        None,
    )
    .await;
    assert_eq!(party["members"][0]["seatId"], seat.as_str());
}

// With asking off, a newcomer sits down at once, as before.
async fn join_seated(app: &axum::Router, t: &Table) -> (String, String) {
    call(
        app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/ask", t.id),
        Some(json!({ "askToJoin": false })),
    )
    .await;
    let seated = join(app, &t.invite, "Tam", "user-1").await;
    call(
        app,
        As::Dm,
        "PATCH",
        &format!("/tables/{}/ask", t.id),
        Some(json!({ "askToJoin": true })),
    )
    .await;
    seated
}

#[tokio::test]
async fn asking_again_replaces_the_old_request() {
    let app = host();
    let t = asking(&app).await;
    ask(&app, &t.invite, "Wren", "user-2").await;
    ask(&app, &t.invite, "Wren again", "user-2").await;
    let (_, party) = call(
        &app,
        As::Dm,
        "GET",
        &format!("/tables/{}/party", t.id),
        None,
    )
    .await;
    let names: Vec<&str> = party["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Wren again"]);
    // A player can't switch the setting.
    let (status, _) = call(
        &app,
        As::Nobody,
        "PATCH",
        &format!("/tables/{}/ask", t.id),
        Some(json!({ "askToJoin": false })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
