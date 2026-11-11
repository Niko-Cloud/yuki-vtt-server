//! Spell areas on the map: cones, spheres, cubes and lines. Anyone the DM lets (with
//! "templates") places one; whoever placed it, or runs the table, moves or takes it away.

use crate::access;
use crate::error::{AppError, Result};
use crate::ids;
use crate::maps::{TableMap, Template, allow, color, credentials, edit};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

const MOST_AREAS: usize = 100;
const SHAPES: [&str; 4] = ["cone", "sphere", "cube", "line"];

#[derive(Deserialize)]
pub struct NewArea {
    shape: String,
    x: f64,
    y: f64,
    size: u32,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    angle: Option<f64>,
    color: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    hidden: bool,
}

#[derive(Deserialize)]
pub struct AreaChange {
    x: Option<f64>,
    y: Option<f64>,
    size: Option<u32>,
    width: Option<u32>,
    angle: Option<f64>,
    color: Option<String>,
    label: Option<String>,
}

fn spot(x: f64, y: f64) -> Result<()> {
    if x.is_finite()
        && y.is_finite()
        && (0.0..=16_384.0).contains(&x)
        && (0.0..=16_384.0).contains(&y)
    {
        Ok(())
    } else {
        Err(AppError::bad("That's off the map"))
    }
}

fn size(feet: u32) -> Result<u32> {
    if (5..=1000).contains(&feet) {
        Ok(feet)
    } else {
        Err(AppError::bad("A spell area is 5 to 1000 feet"))
    }
}

fn width(feet: u32) -> Result<u32> {
    if (5..=100).contains(&feet) {
        Ok(feet)
    } else {
        Err(AppError::bad("A line is 5 to 100 feet wide"))
    }
}

fn angle(degrees: f64) -> Result<f64> {
    if degrees.is_finite() && (-360.0..=360.0).contains(&degrees) {
        Ok(degrees)
    } else {
        Err(AppError::bad("An area's angle is -360 to 360 degrees"))
    }
}

fn label(text: &str) -> Result<Option<String>> {
    let text = text.trim();
    if text.chars().count() > 60 {
        return Err(AppError::bad("An area's name is up to 60 letters"));
    }
    Ok((!text.is_empty()).then(|| text.to_string()))
}

pub async fn add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
    Json(body): Json<NewArea>,
) -> Result<Json<Value>> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    allow(access.may("templates"), "place spell areas")?;
    if body.hidden {
        allow(access.runs(), "put spell areas on the DM's layer")?;
    }
    if !SHAPES.contains(&body.shape.as_str()) {
        return Err(AppError::bad(
            "A spell area is a cone, sphere, cube or line",
        ));
    }
    spot(body.x, body.y)?;
    let by = if access.runs() {
        "dm".to_string()
    } else {
        access
            .seat_ids
            .first()
            .cloned()
            .unwrap_or_else(|| "guest".into())
    };
    let area = Template {
        id: format!("area-{}", ids::id()),
        shape: body.shape,
        x: body.x,
        y: body.y,
        size: size(body.size)?,
        width: width(body.width.unwrap_or(5))?,
        angle: angle(body.angle.unwrap_or(0.0))?,
        color: color(&body.color)?,
        label: label(body.label.as_deref().unwrap_or(""))?,
        by,
        hidden: body.hidden.then_some(true),
    };
    let made = area.id.clone();
    edit(&state, &access, &id, &scene, |mut map| {
        map.templates.push(area);
        // The oldest give way once a map is full of them.
        let extra = map.templates.len().saturating_sub(MOST_AREAS);
        map.templates.drain(..extra);
        Ok(map)
    })?;
    Ok(Json(json!({ "id": made })))
}

// An area this person may change: their own, or any for whoever runs the table. One on the
// DM's layer doesn't exist for players.
fn own(map: &TableMap, access: &access::Access, area: &str) -> Result<usize> {
    let at = map
        .templates
        .iter()
        .position(|t| t.id == area && (access.runs() || t.hidden != Some(true)))
        .ok_or_else(|| AppError::not_found("That spell area is gone"))?;
    allow(
        access.runs() || access.seat_ids.contains(&map.templates[at].by),
        "change it",
    )?;
    Ok(at)
}

pub async fn change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, area)): Path<(String, String, String)>,
    Json(body): Json<AreaChange>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    edit(&state, &access, &id, &scene, |mut map| {
        let at = own(&map, &access, &area)?;
        let template = &mut map.templates[at];
        let (x, y) = (body.x.unwrap_or(template.x), body.y.unwrap_or(template.y));
        spot(x, y)?;
        (template.x, template.y) = (x, y);
        if let Some(feet) = body.size {
            template.size = size(feet)?;
        }
        if let Some(feet) = body.width {
            template.width = width(feet)?;
        }
        if let Some(degrees) = body.angle {
            template.angle = angle(degrees)?;
        }
        if let Some(value) = &body.color {
            template.color = color(value)?;
        }
        if let Some(text) = &body.label {
            template.label = label(text)?;
        }
        Ok(map)
    })
}

pub async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene, area)): Path<(String, String, String)>,
) -> Result<StatusCode> {
    let access = access::at_table(&state, &id, &credentials(&headers))?;
    edit(&state, &access, &id, &scene, |mut map| {
        let at = own(&map, &access, &area)?;
        map.templates.remove(at);
        Ok(map)
    })
}

pub async fn clear(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, scene)): Path<(String, String)>,
) -> Result<StatusCode> {
    let access = access::runner(&state, &id, &credentials(&headers))?;
    edit(&state, &access, &id, &scene, |mut map| {
        map.templates.clear();
        Ok(map)
    })
}
