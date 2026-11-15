//! Rulers pinned to the map: anyone who may draw pins one, and it stays for everyone until
//! whoever pinned it, or runs the table, takes it off.

use crate::access;
use crate::error::{AppError, Result};
use crate::ids;
use crate::maps::{Ruler, allow, credentials, edit};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

const MOST_RULERS: usize = 20;

#[derive(Deserialize)]
pub struct NewRuler {
    points: Vec<[f64; 2]>,
}

pub async fn pin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewRuler>,
) -> Result<Json<Value>> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("draw"), "pin rulers")?;
    let on_map = |&[x, y]: &[f64; 2]| {
        x.is_finite()
            && y.is_finite()
            && (0.0..=16_384.0).contains(&x)
            && (0.0..=16_384.0).contains(&y)
    };
    if !(2..=20).contains(&body.points.len()) || !body.points.iter().all(on_map) {
        return Err(AppError::bad(
            "A ruler runs through 2 to 20 spots on the map",
        ));
    }
    let by = if access.runs() {
        "dm".to_string()
    } else {
        access
            .seat_ids
            .first()
            .cloned()
            .unwrap_or_else(|| "guest".into())
    };
    let ruler = Ruler {
        id: format!("ruler-{}", ids::id()),
        points: body.points,
        by,
    };
    let made = ruler.id.clone();
    edit(&state, &access, &id, &scene, |mut map| {
        map.rulers.push(ruler);
        // The oldest give way past twenty.
        let extra = map.rulers.len().saturating_sub(MOST_RULERS);
        map.rulers.drain(..extra);
        Ok(map)
    })?;
    Ok(Json(json!({ "id": made })))
}

pub async fn unpin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, ruler)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    edit(&state, &access, &id, &scene, |mut map| {
        let at = map
            .rulers
            .iter()
            .position(|r| r.id == ruler)
            .ok_or_else(|| AppError::not_found("That ruler is gone"))?;
        allow(
            access.runs() || access.seat_ids.contains(&map.rulers[at].by),
            "take it off",
        )?;
        map.rulers.remove(at);
        Ok(map)
    })
}
