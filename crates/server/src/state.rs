//! What every request can reach: the database, the host's settings and the live channels.

use crate::delta::ViewVersions;
use crate::events::Events;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub struct Config {
    /// The hash of the DM's key. Whoever holds the key runs every table on this host.
    pub owner_key_hash: String,
    /// The secret shared with the website when the host was paired. The website signs each
    /// character snapshot with it, so a player can't change their sheet on the way here.
    /// `None` takes snapshots unsigned, for trying the host out on its own.
    pub website_secret: Option<Vec<u8>>,
    /// The websites whose pages may talk to this host, like "https://yuki.example".
    pub allowed_origins: Vec<String>,
    /// Where pictures are kept, beside the database.
    pub data_dir: PathBuf,
}

#[derive(Clone)]
pub struct AppState {
    db: Arc<Mutex<Connection>>,
    pub config: Arc<Config>,
    pub events: Events,
    /// Recent copies of the table per viewer, so a screen gets only what changed.
    pub versions: ViewVersions,
}

impl AppState {
    pub fn new(connection: Connection, config: Config) -> Self {
        Self {
            db: Arc::new(Mutex::new(connection)),
            config: Arc::new(config),
            events: Events::default(),
            versions: ViewVersions::default(),
        }
    }

    /// The database, for the length of one request's work. Never held across an `.await`.
    pub fn db(&self) -> MutexGuard<'_, Connection> {
        // A request that panicked mid-write leaves SQLite's own transaction undone, so the
        // connection is still good to use.
        self.db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
