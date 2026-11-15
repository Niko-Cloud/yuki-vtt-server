//! A signed character works once, for one table or seat, and only fresh.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

// The same copy with one field changed and signed again, as if the website had made it so.
fn resigned(body: &Value, field: &str, value: Value) -> Value {
    let mut snapshot: Value = serde_json::from_str(body["snapshot"].as_str().unwrap()).unwrap();
    snapshot[field] = value;
    sign(&snapshot.to_string())
}

async fn sit(app: &axum::Router, invite: &str, body: Value) -> StatusCode {
    call(
        app,
        As::Nobody,
        "POST",
        &format!("/join/{invite}"),
        Some(body),
    )
    .await
    .0
}

async fn save(app: &axum::Router, token: &str, body: &Value) -> StatusCode {
    call(app, As::Seat(token), "PUT", "/seat", Some(body.clone()))
        .await
        .0
}

#[tokio::test]
async fn a_join_sits_down_once_at_its_own_table_and_only_fresh() {
    let app = host();
    let t = table(&app).await;
    let other = table(&app).await;
    let body = signed("Tam", "user-1", 18, fresh(), For::Join(&t.id));
    assert_eq!(
        sit(&app, &t.invite, body.clone()).await,
        StatusCode::CREATED
    );
    assert_eq!(
        sit(&app, &t.invite, body.clone()).await,
        StatusCode::CONFLICT
    );

    let elsewhere = signed("Tam", "user-1", 18, fresh(), For::Join(&t.id));
    assert_eq!(
        sit(&app, &other.invite, elsewhere).await,
        StatusCode::FORBIDDEN
    );
    let stale = resigned(&body, "issuedAt", json!(ids_now() - 60 * 60_000));
    assert_eq!(sit(&app, &t.invite, stale).await, StatusCode::FORBIDDEN);
    let untimed = resigned(&body, "issuedAt", Value::Null);
    assert_eq!(sit(&app, &t.invite, untimed).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_save_goes_to_its_own_seat_and_never_back_in_time() {
    let app = host();
    let t = table(&app).await;
    let (tam_seat, tam) = join(&app, &t.invite, "Tam", "user-1").await;
    let (wren_seat, wren) = join(&app, &t.invite, "Wren", "user-2").await;

    let mut hurt = fresh();
    hurt["currentHitPoints"] = json!(9);
    let older = signed("Tam", "user-1", 18, fresh(), For::Seat(&tam_seat));
    let newer = signed("Tam", "user-1", 18, hurt, For::Seat(&tam_seat));
    assert_eq!(save(&app, &tam, &newer).await, StatusCode::OK);
    // Kept from before and sent again: refused, and the seat keeps the newer state.
    assert_eq!(save(&app, &tam, &older).await, StatusCode::CONFLICT);
    assert_eq!(save(&app, &tam, &newer).await, StatusCode::CONFLICT);
    let (_, pulled) = call(&app, As::Seat(&tam), "GET", "/seat", None).await;
    assert_eq!(pulled["state"]["currentHitPoints"], 9);
    // The seat token goes in a header; an address with it in finds nothing.
    let (status, _) = call(&app, As::Nobody, "GET", &format!("/seats/{tam}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, As::Nobody, "GET", "/seat", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Tam's copy doesn't fit Wren's seat, and an old one isn't taken.
    let tams = signed("Tam", "user-1", 18, fresh(), For::Seat(&tam_seat));
    assert_eq!(save(&app, &wren, &tams).await, StatusCode::FORBIDDEN);
    let stale = resigned(
        &signed("Wren", "user-2", 18, fresh(), For::Seat(&wren_seat)),
        "issuedAt",
        json!(ids_now() - 60 * 60_000),
    );
    assert_eq!(save(&app, &wren, &stale).await, StatusCode::FORBIDDEN);
    let fine = signed("Wren", "user-2", 18, fresh(), For::Seat(&wren_seat));
    assert_eq!(save(&app, &wren, &fine).await, StatusCode::OK);
}

fn ids_now() -> i64 {
    yuki_vtt_server::ids::now()
}
