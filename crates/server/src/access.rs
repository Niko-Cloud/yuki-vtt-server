//! Who is asking: the DM (with the host's key), or someone with a seat at the table (with the
//! seat token their browser keeps). A co-DM is a seat the DM made one.

use crate::error::{AppError, Result};
use crate::ids;
use crate::state::AppState;
use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use rusqlite::OptionalExtension;
use serde::Serialize;
use sha2::Sha256;

pub const PERMISSIONS: [&str; 4] = ["move-own", "move-any", "tokens", "draw"];

/// How long a media pass lets a page load pictures and the live channel.
pub const MEDIA_PASS_SECONDS: i64 = 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Dm,
    Codm,
    Player,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Access {
    pub role: Role,
    pub name: String,
    pub seat_ids: Vec<String>,
    /// What this viewer may do on the map; whoever runs the table may do it all.
    pub permissions: Vec<String>,
}

impl Access {
    /// The DM or a co-DM.
    pub fn runs(&self) -> bool {
        self.role != Role::Player
    }

    pub fn may(&self, permission: &str) -> bool {
        self.runs() || self.permissions.iter().any(|p| p == permission)
    }
}

/// The credentials a request brought. Pictures and the live channel can't send headers, so
/// they bring `?pass=`, a media pass that runs out: addresses end up in logs and the
/// browser's history, and the DM's key and seat tokens must not.
#[derive(Debug, Default, Clone)]
pub struct Credentials {
    pub key: Option<String>,
    pub seat: Option<String>,
    pub pass: Option<String>,
}

impl Credentials {
    pub fn from(headers: &HeaderMap, pass: Option<String>) -> Self {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let bearer =
            header("authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_string));
        Self {
            key: bearer,
            seat: header("x-seat-token"),
            pass,
        }
    }

    pub fn is_owner(&self, state: &AppState) -> bool {
        self.key
            .as_deref()
            .is_some_and(|key| ids::hash(key) == state.config.owner_key_hash)
    }
}

// Signed with the DM key's hash, so a pass is good for one table (and one seat) on this host
// only, and changing the key ends every pass.
fn media_mac(state: &AppState, table_id: &str, seat_id: &str, expires: i64) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(state.config.owner_key_hash.as_bytes())
        .expect("any key length works");
    mac.update(format!("media:{table_id}:{seat_id}:{expires}").as_bytes());
    mac
}

/// A media pass for one table: "<seat>.<expires>.<signature>", the seat left empty for the
/// DM's own, and when it runs out (milliseconds). A seat's pass stands in for the seat token
/// in addresses, which never runs out.
pub fn media_pass(state: &AppState, table_id: &str, seat_id: Option<&str>) -> (String, i64) {
    let seat_id = seat_id.unwrap_or("");
    let expires = ids::now() + MEDIA_PASS_SECONDS * 1000;
    let signature = media_mac(state, table_id, seat_id, expires)
        .finalize()
        .into_bytes();
    (
        format!("{seat_id}.{expires}.{}", hex::encode(signature)),
        expires,
    )
}

/// Whose pass this is, while it still works: the DM's (None) or a seat's.
fn pass_holder(state: &AppState, table_id: &str, pass: &str) -> Option<Option<String>> {
    let mut parts = pass.splitn(3, '.');
    let (seat_id, expires, signature) = (parts.next()?, parts.next()?, parts.next()?);
    let expires = expires.parse::<i64>().ok()?;
    let signature = hex::decode(signature).ok()?;
    let good = expires > ids::now()
        && media_mac(state, table_id, seat_id, expires)
            .verify_slice(&signature)
            .is_ok();
    good.then(|| (!seat_id.is_empty()).then(|| seat_id.to_string()))
}

/// Only the DM: making, renaming and deleting tables, roles, bans, the invite.
pub fn owner(state: &AppState, credentials: &Credentials) -> Result<()> {
    if credentials.is_owner(state) {
        Ok(())
    } else {
        Err(AppError::new(
            axum::http::StatusCode::UNAUTHORIZED,
            "Only the DM's key can do that",
        ))
    }
}

/// Who this is at the table. Anyone else gets "no such table", so they learn nothing.
pub fn at_table(state: &AppState, table_id: &str, credentials: &Credentials) -> Result<Access> {
    let db = state.db();
    let missing = || AppError::not_found(format!("No table with id {table_id}"));
    let dm_name: String = db
        .query_row(
            "SELECT dm_name FROM tables WHERE id = ?1",
            [table_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(missing)?;
    let held = credentials
        .pass
        .as_deref()
        .and_then(|pass| pass_holder(state, table_id, pass));
    if credentials.is_owner(state) || held == Some(None) {
        return Ok(Access {
            role: Role::Dm,
            name: dm_name,
            seat_ids: vec![],
            permissions: all(),
        });
    }
    // A seat by its token, or by its pass; a seat the DM removed takes its passes with it.
    let (column, value) = match (&credentials.seat, held.flatten()) {
        (Some(token), _) => ("token_hash", ids::hash(token)),
        (None, Some(seat_id)) => ("id", seat_id),
        (None, None) => return Err(missing()),
    };
    let seat: Option<(String, String, String, String)> = db
        .query_row(
            &format!("SELECT id, role, permissions, snapshot FROM seats WHERE table_id = ?1 AND {column} = ?2"),
            [table_id, &value],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let (seat_id, role, permissions, snapshot) = seat.ok_or_else(missing)?;
    let name = serde_json::from_str::<serde_json::Value>(&snapshot)?["summary"]["name"]
        .as_str()
        .unwrap_or("Guest")
        .to_string();
    if role == "codm" {
        return Ok(Access {
            role: Role::Codm,
            name,
            seat_ids: vec![],
            permissions: all(),
        });
    }
    Ok(Access {
        role: Role::Player,
        name,
        seat_ids: vec![seat_id],
        permissions: serde_json::from_str(&permissions)?,
    })
}

/// The DM or a co-DM, or a refusal.
pub fn runner(state: &AppState, table_id: &str, credentials: &Credentials) -> Result<Access> {
    let access = at_table(state, table_id, credentials)?;
    if access.runs() {
        Ok(access)
    } else {
        Err(AppError::forbidden("Only the DM can do that"))
    }
}

fn all() -> Vec<String> {
    PERMISSIONS.iter().map(|p| p.to_string()).collect()
}
