//! Yuki VTT Server: hosts Yuki Char Maker game tables on the DM's computer. The website's
//! pages talk to it with the same routes as the website's own tables, under `/api`.

pub mod access;
pub mod db;
pub mod error;
pub mod events;
pub mod ids;
pub mod maps;
pub mod pictures;
pub mod play;
pub mod seats;
pub mod secrets;
pub mod snapshot;
pub mod state;
pub mod tables;
pub mod tunnel;
pub mod website;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderName, HeaderValue, Method};
use axum::routing::{get, patch, post};
use serde_json::json;
use state::AppState;
use tower_http::cors::{AllowOrigin, CorsLayer};

// A character snapshot with its sheet is the biggest thing sent as JSON.
const BODY_LIMIT: usize = 2 * 1024 * 1024;

pub fn app(state: AppState) -> Router {
    let origins: Vec<HeaderValue> = state
        .config
        .allowed_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();
    // Only the paired website's pages may call the host from a browser.
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            HeaderName::from_static("authorization"),
            HeaderName::from_static("content-type"),
            HeaderName::from_static("x-seat-token"),
        ]);

    let api = Router::new()
        .route("/health", get(|| async { axum::Json(json!({ "ok": true, "app": "yuki-vtt-server", "version": env!("CARGO_PKG_VERSION") })) }))
        .route("/tables", get(tables::list).post(tables::create))
        .route("/tables/{id}", get(play::view).patch(tables::rename).delete(tables::delete))
        .route("/tables/{id}/events", get(events::open))
        .route("/tables/{id}/party", get(tables::party))
        .route("/tables/{id}/invite", post(tables::renew_invite))
        .route("/tables/{id}/lock", patch(tables::lock))
        .route("/tables/{id}/log", post(play::post))
        .route("/tables/{id}/log/{entry}/reveal", post(play::reveal))
        .route("/tables/{id}/roll", post(play::roll))
        .route("/tables/{id}/seats/{seat}", get(play::seat_sheet).delete(tables::remove_seat))
        .route("/tables/{id}/seats/{seat}/change", post(tables::change))
        .route("/tables/{id}/seats/{seat}/role", patch(tables::set_role))
        .route("/tables/{id}/seats/{seat}/permissions", patch(tables::set_permissions))
        .route("/tables/{id}/seats/{seat}/ban", post(tables::ban))
        .route("/tables/{id}/bans/{player}", axum::routing::delete(tables::unban))
        .route("/tables/{id}/encounter", post(play::start).delete(play::end))
        .route("/tables/{id}/encounter/combatants", post(play::add))
        .route("/tables/{id}/encounter/combatants/{combatant}", patch(play::set_initiative).delete(play::remove))
        .route("/tables/{id}/encounter/next", post(play::next))
        .route("/tables/{id}/encounter/previous", post(play::previous))
        .route("/tables/{id}/scenes", post(maps::create))
        .route("/tables/{id}/scenes/{scene}", patch(maps::update).delete(maps::remove))
        .route("/tables/{id}/scenes/{scene}/show", post(maps::show))
        .route(
            "/tables/{id}/scenes/{scene}/image",
            get(pictures::map).put(pictures::set_map).layer(DefaultBodyLimit::max(pictures::MAP_LIMIT + 1024)),
        )
        .route("/tables/{id}/scenes/{scene}/tokens", post(maps::add_token))
        .route("/tables/{id}/scenes/{scene}/tokens/party", post(maps::place_party))
        .route("/tables/{id}/scenes/{scene}/tokens/{token}", patch(maps::change_token).delete(maps::remove_token))
        .route("/tables/{id}/scenes/{scene}/props", post(maps::add_prop))
        .route("/tables/{id}/scenes/{scene}/props/{prop}", patch(maps::change_prop).delete(maps::remove_prop))
        .route("/tables/{id}/scenes/{scene}/drawings", post(maps::add_drawing).delete(maps::clear_drawings))
        .route("/tables/{id}/scenes/{scene}/drawings/{drawing}", axum::routing::delete(maps::remove_drawing))
        .route("/tables/{id}/ping", post(maps::ping))
        .route("/tables/{id}/assets", post(pictures::add).layer(DefaultBodyLimit::max(pictures::LIBRARY_LIMIT + 1024)))
        .route("/tables/{id}/assets/{asset}", get(pictures::picture).delete(pictures::remove))
        .route("/tables/{id}/handout", axum::routing::put(pictures::show).delete(pictures::hide))
        .route("/join/{invite}", get(seats::invite).post(seats::join))
        .route("/seats/{token}", get(seats::pull).put(seats::push).delete(seats::leave));

    Router::new()
        .nest("/api", api)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(cors)
        .with_state(state)
}
