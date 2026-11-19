//! What leaves the DM's home upload: as little as the table needs.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use flate2::read::GzDecoder;
use http_body_util::BodyExt;
use std::io::Read;
use tower::ServiceExt;

const PNG: &[u8] =
    b"\x89PNG\r\n\x1a\n-a-picture-that-is-already-compressed-and-long-enough-to-matter";

async fn fetch_gzip(app: &axum::Router, path: &str) -> (StatusCode, Option<String>, Vec<u8>) {
    let request = Request::builder()
        .uri(format!("/api{path}"))
        .header("authorization", format!("Bearer {KEY}"))
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let encoding = response
        .headers()
        .get(header::CONTENT_ENCODING)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, encoding, bytes.to_vec())
}

#[tokio::test]
async fn compresses_the_table_but_not_its_pictures() {
    let app = host();
    let t = table(&app).await;
    for i in 0..20 {
        let roll = serde_json::json!({ "label": format!("Attack {i}"), "expression": "1d20+5" });
        call(
            &app,
            As::Dm,
            "POST",
            &format!("/tables/{}/roll", t.id),
            Some(roll),
        )
        .await;
    }
    let (status, encoding, bytes) = fetch_gzip(&app, &format!("/tables/{}", t.id)).await;
    assert_eq!(
        (status, encoding.as_deref()),
        (StatusCode::OK, Some("gzip"))
    );
    let mut text = String::new();
    GzDecoder::new(bytes.as_slice())
        .read_to_string(&mut text)
        .unwrap();
    let view: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(view["party"]["id"], t.id.as_str());
    assert!(bytes.len() * 3 < text.len(), "the table shrinks well");

    let (_, added) = upload(
        &app,
        As::Dm,
        "POST",
        &format!(
            "/tables/{}/assets?kind=token&name=Ogre&width=64&height=64",
            t.id
        ),
        PNG,
    )
    .await;
    let picture = format!("/tables/{}/assets/{}", t.id, added["id"].as_str().unwrap());
    let (status, encoding, bytes) = fetch_gzip(&app, &picture).await;
    assert_eq!((status, encoding), (StatusCode::OK, None));
    assert_eq!(bytes, PNG);
}

#[tokio::test]
async fn sends_the_newest_30_log_entries_and_older_ones_a_page_at_a_time() {
    let app = host();
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let roll = |label: String, hidden: bool| serde_json::json!({ "label": label, "expression": "1d20", "hidden": hidden });
    let at = format!("/tables/{}/roll", t.id);
    call(
        &app,
        As::Dm,
        "POST",
        &at,
        Some(roll("Secret first".into(), true)),
    )
    .await;
    for i in 1..=40 {
        call(
            &app,
            As::Dm,
            "POST",
            &at,
            Some(roll(format!("Roll {i}"), false)),
        )
        .await;
    }
    let labels = |entries: &serde_json::Value| -> Vec<String> {
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["data"]["label"].as_str().unwrap().to_string())
            .collect()
    };

    let (_, view) = call(&app, As::Dm, "GET", &format!("/tables/{}", t.id), None).await;
    let shown = labels(&view["log"]);
    assert_eq!(shown.len(), 30);
    assert_eq!(
        (shown[0].as_str(), shown[29].as_str()),
        ("Roll 11", "Roll 40")
    );
    assert_eq!(view["logMore"], true);

    let first = view["log"][0]["id"].as_str().unwrap();
    let back = format!("/tables/{}/log?before={first}", t.id);
    let (_, older) = call(&app, As::Dm, "GET", &back, None).await;
    let mut expected = vec!["Secret first".to_string()];
    expected.extend((1..=10).map(|i| format!("Roll {i}")));
    assert_eq!(labels(&older["entries"]), expected);
    assert_eq!(older["more"], false);

    // A player reads back too, without the DM's hidden roll.
    let (_, theirs) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("/tables/{}", t.id),
        None,
    )
    .await;
    let first = theirs["log"][0]["id"].as_str().unwrap();
    let back = format!("/tables/{}/log?before={first}", t.id);
    let (_, older) = call(&app, As::Seat(&token), "GET", &back, None).await;
    assert_eq!(labels(&older["entries"]).len(), 10);
    assert!(!labels(&older["entries"]).contains(&"Secret first".to_string()));
    let (status, _) = call(&app, As::Nobody, "GET", &back, None).await;
    assert!(status.is_client_error());
    let (status, _) = call(&app, As::Dm, "GET", &format!("/tables/{}/log", t.id), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn answers_a_screen_that_has_the_table_with_only_what_changed() {
    use yuki_vtt_server::delta::apply;
    let app = host();
    let t = table(&app).await;
    let (_, token) = join(&app, &t.invite, "Tam", "user-1").await;
    let (_, made) = call(
        &app,
        As::Dm,
        "POST",
        &format!("/tables/{}/scenes", t.id),
        Some(serde_json::json!({ "name": "Cave" })),
    )
    .await;
    let scene = made["id"].as_str().unwrap();
    let ogre = serde_json::json!({ "name": "Ogre", "x": 2, "y": 2 });
    let tokens = format!("/tables/{}/scenes/{scene}/tokens", t.id);
    call(&app, As::Dm, "POST", &tokens, Some(ogre)).await;
    let path = format!("/tables/{}", t.id);
    let (_, mut had) = call(&app, As::Dm, "GET", &path, None).await;
    let version = had["version"].as_str().unwrap().to_string();
    assert_eq!(version.len(), 16);
    had.as_object_mut().unwrap().remove("version");

    let moved = format!(
        "{tokens}/{}",
        had["scene"]["tokens"][0]["id"].as_str().unwrap()
    );
    let x = serde_json::json!({ "x": 5 });
    call(&app, As::Dm, "PATCH", &moved, Some(x)).await;
    let (_, changed) = call(&app, As::Dm, "GET", &format!("{path}?base={version}"), None).await;
    assert_eq!(changed.get("party"), None);
    assert_eq!(changed["base"], version.as_str());
    assert!(changed["delta"].to_string().len() * 5 < had.to_string().len());
    let (_, mut whole) = call(&app, As::Dm, "GET", &path, None).await;
    assert_eq!(changed["version"], whole["version"]);
    let now = whole["version"].as_str().unwrap().to_string();
    whole.as_object_mut().unwrap().remove("version");
    assert_eq!(apply(&had, changed.get("delta")), whole);

    // Nothing new: just the version. An unknown one, or another viewer's, gets the whole table.
    let (_, same) = call(&app, As::Dm, "GET", &format!("{path}?base={now}"), None).await;
    assert_eq!(same, serde_json::json!({ "version": now, "base": now }));
    let (_, unknown) = call(&app, As::Dm, "GET", &format!("{path}?base=nope"), None).await;
    assert!(unknown.get("party").is_some());
    let (_, theirs) = call(
        &app,
        As::Seat(&token),
        "GET",
        &format!("{path}?base={now}"),
        None,
    )
    .await;
    assert_eq!(theirs["you"]["role"], "player");
}
