//! The host keeps everything in one SQLite file in its data folder. The game's own shapes
//! (an encounter, a log entry's data, a seat's character snapshot) are stored as JSON text.

use rusqlite::Connection;
use std::path::Path;

// Each step brings an older file up to date; `user_version` says how far a file has come.
const STEPS: &[&str] = &[
    r#"
    CREATE TABLE tables (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        edition TEXT NOT NULL,
        dm_name TEXT NOT NULL,
        invite_token TEXT NOT NULL UNIQUE,
        locked INTEGER NOT NULL DEFAULT 0,
        banned TEXT NOT NULL DEFAULT '[]',
        encounter TEXT,
        handout TEXT,
        active_scene_id TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE TABLE seats (
        id TEXT PRIMARY KEY,
        table_id TEXT NOT NULL REFERENCES tables(id) ON DELETE CASCADE,
        token_hash TEXT NOT NULL UNIQUE,
        role TEXT NOT NULL DEFAULT 'player',
        permissions TEXT NOT NULL DEFAULT '["move-own"]',
        player_id TEXT,
        snapshot TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE INDEX seats_by_table ON seats(table_id, created_at);
    CREATE TABLE log (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        id TEXT NOT NULL UNIQUE,
        table_id TEXT NOT NULL REFERENCES tables(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        author TEXT NOT NULL,
        data TEXT NOT NULL,
        visible_to TEXT NOT NULL DEFAULT '[]',
        created_at INTEGER NOT NULL
    );
    CREATE INDEX log_by_table ON log(table_id, seq);
"#,
    r#"
    CREATE TABLE scenes (
        id TEXT PRIMARY KEY,
        table_id TEXT NOT NULL REFERENCES tables(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        position INTEGER NOT NULL,
        data TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX scenes_by_table ON scenes(table_id, position);
    CREATE TABLE assets (
        id TEXT PRIMARY KEY,
        table_id TEXT NOT NULL REFERENCES tables(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        name TEXT NOT NULL,
        mime TEXT NOT NULL,
        width INTEGER NOT NULL,
        height INTEGER NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX assets_by_table ON assets(table_id, created_at);
"#,
    r#"
    ALTER TABLE tables ADD COLUMN ask_to_join INTEGER NOT NULL DEFAULT 1;
    CREATE TABLE join_requests (
        id TEXT PRIMARY KEY,
        table_id TEXT NOT NULL REFERENCES tables(id) ON DELETE CASCADE,
        token_hash TEXT NOT NULL UNIQUE,
        player_id TEXT,
        snapshot TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX join_requests_by_table ON join_requests(table_id, created_at);
"#,
    r#"
    CREATE TABLE used_joins (
        signature TEXT PRIMARY KEY,
        used_at INTEGER NOT NULL
    );
"#,
    r#"
    ALTER TABLE tables ADD COLUMN shared_sight INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE explored (
        scene_id TEXT NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
        table_id TEXT NOT NULL REFERENCES tables(id) ON DELETE CASCADE,
        viewer TEXT NOT NULL,
        cells TEXT NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (scene_id, viewer)
    );
    CREATE INDEX explored_by_table ON explored(table_id);
"#,
    r#"
    ALTER TABLE scenes ADD COLUMN folder TEXT;
"#,
    r#"
    ALTER TABLE tables ADD COLUMN prefabs TEXT;
"#,
];

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    prepare(connection)
}

/// A database that lives only in memory, for tests.
pub fn in_memory() -> rusqlite::Result<Connection> {
    prepare(Connection::open_in_memory()?)
}

fn prepare(mut connection: Connection) -> rusqlite::Result<Connection> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let at: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for (i, step) in (0i64..).zip(STEPS).skip(at.max(0) as usize) {
        let tx = connection.transaction()?;
        tx.execute_batch(step)?;
        tx.pragma_update(None, "user_version", i + 1)?;
        tx.commit()?;
    }
    Ok(connection)
}
