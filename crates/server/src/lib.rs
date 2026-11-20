//! Yuki VTT Server: hosts Yuki Char Maker game tables on the DM's computer. The website's
//! pages talk to it with the same routes as the website's own tables, under `/api`.

pub mod access;
pub mod areas;
pub mod awake;
pub mod backup;
pub mod cloudflared;
pub mod connection;
pub mod db;
pub mod delta;
pub mod error;
pub mod events;
pub mod fog;
pub mod host;
pub mod ids;
pub mod library;
pub mod limits;
pub mod maps;
pub mod pictures;
pub mod play;
pub mod requests;
pub mod rulers;
pub mod seats;
pub mod secrets;
pub mod served;
pub mod sight;
pub mod snapshot;
pub mod state;
pub mod tables;
pub mod tunnel;
pub mod web_picture;
pub mod website;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderName, HeaderValue, Method};
use axum::routing::{get, patch, post};
use serde_json::json;
use state::AppState;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::cors::{AllowOrigin, CorsLayer};

// A character snapshot with its sheet is the biggest thing sent as JSON.
const BODY_LIMIT: usize = 2 * 1024 * 1024;

// Answers go out over the DM's home upload, so the table's JSON is squeezed first. Pictures,
// clips, tracks and backups are compressed already and go as they are.
fn compression() -> CompressionLayer<impl Predicate> {
    let predicate = DefaultPredicate::new()
        .and(NotForContentType::const_new("video/"))
        .and(NotForContentType::const_new("audio/"))
        .and(NotForContentType::const_new("application/gzip"));
    CompressionLayer::new().compress_when(predicate)
}

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
        .route("/tables/{id}/media-token", post(tables::media_token))
        .route("/tables/{id}/seat-pass", post(tables::seat_pass))
        .route("/tables/{id}/lock", patch(tables::lock))
        .route("/tables/{id}/guests", patch(tables::set_guests))
        .route("/tables/{id}/log", get(play::older_log).post(play::post))
        .route("/tables/{id}/log/{entry}/reveal", post(play::reveal))
        .route("/tables/{id}/roll", post(play::roll))
        .route("/tables/{id}/seats/{seat}", get(play::seat_sheet).delete(tables::remove_seat))
        .route("/tables/{id}/seats/{seat}/change", post(tables::change))
        .route("/tables/{id}/seats/{seat}/role", patch(tables::set_role))
        .route("/tables/{id}/seats/{seat}/permissions", patch(tables::set_permissions))
        .route("/tables/{id}/seats/{seat}/ban", post(tables::ban))
        .route("/tables/{id}/bans/{player}", axum::routing::delete(tables::unban))
        .route("/tables/{id}/encounter", post(play::start).delete(play::end).patch(play::change_encounter))
        .route("/tables/{id}/encounter/roll", post(play::reroll))
        .route("/tables/{id}/encounter/combatants", post(play::add))
        .route("/tables/{id}/encounter/combatants/{combatant}", patch(play::change_combatant).delete(play::remove))
        .route("/tables/{id}/encounter/next", post(play::next))
        .route("/tables/{id}/encounter/previous", post(play::previous))
        .route("/tables/{id}/scenes", post(maps::create))
        .route("/tables/{id}/scenes/{scene}", patch(maps::update).delete(maps::remove))
        .route("/tables/{id}/scenes/{scene}/show", post(maps::show))
        .route("/tables/{id}/scenes/{scene}/copy", post(maps::copy))
        .route("/tables/{id}/scenes/{scene}/preload", post(maps::preload))
        .route("/tables/{id}/prefabs", post(maps::add_prefab))
        .route("/tables/{id}/prefabs/{prefab}", axum::routing::delete(maps::remove_prefab))
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
        .route("/tables/{id}/scenes/{scene}/drawings/{drawing}", axum::routing::delete(maps::remove_drawing).patch(maps::change_drawing))
        .route("/tables/{id}/ping", post(maps::ping))
        .route("/tables/{id}/mark", post(maps::mark))
        .route("/tables/{id}/view", post(maps::view))
        .route(
            "/tables/{id}/xcard",
            post(play::xcard).delete(play::resume_after_xcard),
        )
        .route("/tables/{id}/timer", axum::routing::put(play::set_timer).delete(play::clear_timer))
        .route("/tables/{id}/loot", axum::routing::put(play::set_loot))
        .route("/tables/{id}/journal", axum::routing::put(play::set_journal))
        // Spell areas, placed by those the DM lets.
        .route("/tables/{id}/scenes/{scene}/templates", post(areas::add).delete(areas::clear))
        .route("/tables/{id}/scenes/{scene}/templates/{area}", patch(areas::change).delete(areas::remove))
        // Rulers pinned to the map, by those who may draw.
        .route("/tables/{id}/scenes/{scene}/rulers", post(rulers::pin))
        .route("/tables/{id}/scenes/{scene}/rulers/{ruler}", axum::routing::delete(rulers::unpin))
        // Fog of war: the DM's walls, doors, lights and notes; doors players stand next to.
        .route("/tables/{id}/sight", patch(fog::set_sight))
        .route("/tables/{id}/scenes/{scene}/lighting", axum::routing::put(fog::set_lighting))
        .route("/tables/{id}/scenes/{scene}/doors/{wall}", post(fog::set_door))
        .route("/tables/{id}/scenes/{scene}/notes", post(fog::add_note))
        .route("/tables/{id}/scenes/{scene}/notes/{note}", axum::routing::delete(fog::remove_note))
        .route("/tables/{id}/scenes/{scene}/explored", get(fog::explored).post(fog::explore).delete(fog::forget_explored))
        .route("/tables/{id}/assets", post(pictures::add).layer(DefaultBodyLimit::max(pictures::VIDEO_LIBRARY_LIMIT.max(pictures::AUDIO_LIMIT) + 1024)))
        .route("/tables/{id}/assets/{asset}", get(pictures::picture).patch(pictures::change).delete(pictures::remove))
        .route("/tables/{id}/web-picture", post(web_picture::web_picture))
        .route("/tables/{id}/library/open", post(library::open))
        .route("/tables/{id}/handout", axum::routing::put(pictures::show).delete(pictures::hide))
        .route("/tables/{id}/music", axum::routing::put(pictures::play_music).delete(pictures::stop_music))
        .route("/tables/{id}/background", axum::routing::put(pictures::set_background).delete(pictures::clear_background))
        .route("/tables/{id}/ask", patch(requests::set_asking))
        .route("/tables/{id}/requests/{request}/{choice}", post(requests::decide))
        // The seat token comes in the x-seat-token header, never in the address.
        .route("/seat/status", get(requests::status))
        .route("/join/{invite}", get(seats::invite).post(seats::join))
        .route("/join/{invite}/screen", post(seats::join_screen))
        .route("/seat", get(seats::pull).put(seats::push).delete(seats::leave));

    Router::new()
        .nest("/api", api)
        .layer(axum::middleware::from_fn_with_state(
            limits::Limits::default(),
            limits::check,
        ))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(cors)
        .layer(compression())
        .with_state(state)
}
