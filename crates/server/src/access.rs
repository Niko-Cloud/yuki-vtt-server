//! Who is asking: the DM (with the host's key), or someone with a seat at the table (with the
//! seat token their browser keeps). A co-DM is a seat the DM made one.

use crate::error::{AppError, Result};
use crate::ids;
use crate::state::AppState;
use axum::http::HeaderMap;
use rusqlite::OptionalExtension;
use serde::Serialize;

pub const PERMISSIONS: [&str; 4] = ["move-own", "move-any", "tokens", "draw"];

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
/// they may come in the query as `?key=` or `?seat=`.
#[derive(Debug, Default, Clone)]
pub struct Credentials {
    pub key: Option<String>,
    pub seat: Option<String>,
}

impl Credentials {
    pub fn from(headers: &HeaderMap, key: Option<String>, seat: Option<String>) -> Self {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let bearer =
            header("authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_string));
        Self {
            key: bearer.or(key),
            seat: header("x-seat-token").or(seat),
        }
    }

    pub fn is_owner(&self, state: &AppState) -> bool {
        self.key
            .as_deref()
            .is_some_and(|key| ids::hash(key) == state.config.owner_key_hash)
    }
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
    if credentials.is_owner(state) {
        return Ok(Access {
            role: Role::Dm,
            name: dm_name,
            seat_ids: vec![],
            permissions: all(),
        });
    }
    let Some(token) = &credentials.seat else {
        return Err(missing());
    };
    let seat: Option<(String, String, String, String)> = db
        .query_row(
            "SELECT id, role, permissions, snapshot FROM seats WHERE table_id = ?1 AND token_hash = ?2",
            [table_id, &ids::hash(token)],
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
