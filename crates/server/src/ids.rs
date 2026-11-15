//! Ids, secret tokens and the time, all in one place.

use rand::Rng;
use rand::distr::Alphanumeric;
use sha2::{Digest, Sha256};

/// A short random id for a table, a seat or a log entry.
pub fn id() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(20)
        .map(char::from)
        .collect()
}

/// A secret: an invite code, a seat token or the DM's key. `rand::rng()` is seeded from the
/// operating system and is safe for secrets.
pub fn secret() -> String {
    let bytes: [u8; 24] = rand::rng().random();
    hex::encode(bytes)
}

/// Only the hash of a secret is stored, so a copied database file lets no one in.
pub fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// Milliseconds since 1970.
pub fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A stored time as the website writes times: "2026-11-11T20:00:00.000Z".
pub fn iso(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
