//! A table in one file, to keep somewhere safe or move to another computer: its maps, tokens,
//! seats, log and pictures. The file is a .yukitable (a gzipped tar): `table.json` with every
//! row, and the table's pictures beside it.

use crate::db;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use rusqlite::types::{Value as Sql, ValueRef};
use rusqlite::{Connection, params_from_iter};
use serde_json::{Map, Number, Value, json};
use std::io::Read;
use std::path::Path;

const FORMAT: u64 = 1;
// A table's pictures are at most 200 of 8 MB; a file bigger than this isn't one of ours.
const BIGGEST: u64 = 2 * 1024 * 1024 * 1024;
// The table's rows, without pictures: far more than any real table's log and maps.
const TABLE_LIMIT: u64 = 256 * 1024 * 1024;

// The rows a table owns, and the columns kept for each. Only these names ever reach SQL.
const PARTS: [(&str, &[&str]); 6] = [
    (
        "tables",
        &[
            "id",
            "name",
            "edition",
            "dm_name",
            "invite_token",
            "locked",
            "banned",
            "encounter",
            "handout",
            "active_scene_id",
            "created_at",
            "updated_at",
            "ask_to_join",
            "shared_sight",
            "prefabs",
        ],
    ),
    (
        "seats",
        &[
            "id",
            "table_id",
            "token_hash",
            "role",
            "permissions",
            "player_id",
            "snapshot",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "scenes",
        &["id", "table_id", "name", "position", "data", "created_at"],
    ),
    // What players explored of each map with fog; after the scenes it belongs to.
    (
        "explored",
        &["scene_id", "table_id", "viewer", "cells", "updated_at"],
    ),
    (
        "assets",
        &[
            "id",
            "table_id",
            "kind",
            "name",
            "mime",
            "width",
            "height",
            "created_at",
        ],
    ),
    (
        "log",
        &[
            "id",
            "table_id",
            "kind",
            "author",
            "data",
            "visible_to",
            "created_at",
        ],
    ),
];

fn problem(what: &str) -> impl Fn(std::io::Error) -> String + '_ {
    move |error| format!("{what}: {error}")
}

fn as_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => json!(n),
        ValueRef::Real(n) => Number::from_f64(n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(text) => json!(String::from_utf8_lossy(text)),
        ValueRef::Blob(_) => Value::Null,
    }
}

// Columns kept as numbers; every other one is text.
const NUMBERS: [&str; 9] = [
    "locked",
    "ask_to_join",
    "shared_sight",
    "created_at",
    "updated_at",
    "position",
    "width",
    "height",
    "seq",
];

// Text that must read back as `T`.
fn holds<T: serde::de::DeserializeOwned>(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| serde_json::from_str::<T>(text).is_ok())
}

/// Whether a backed-up value is what the host reads back from that column. SQLite keeps
/// whatever it is given, so a damaged or made-up backup would otherwise give a table that
/// breaks each time it is opened.
fn fits(column: &str, value: &Value) -> bool {
    use crate::snapshot::Snapshot;
    match column {
        c if NUMBERS.contains(&c) => value.is_i64() || value.is_boolean(),
        "snapshot" => holds::<Snapshot>(value),
        "permissions" | "visible_to" => holds::<Vec<String>>(value),
        "banned" => holds::<Vec<Value>>(value),
        "encounter" => value.is_null() || holds::<yuki_vtt_rules::encounter::Encounter>(value),
        "handout" => value.is_null() || holds::<Value>(value),
        "prefabs" => value.is_null() || holds::<Vec<crate::maps::Prefab>>(value),
        "active_scene_id" | "player_id" => value.is_null() || value.is_string(),
        _ => value.is_string(),
    }
}

// The parts of a row that only make sense together.
fn row_fits(part: &str, row: &Value) -> bool {
    match part {
        "scenes" => holds::<crate::maps::TableMap>(&row["data"]),
        "log" => {
            let data = row["data"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok());
            match data {
                Some(data) if row["kind"] == "change" => serde_json::from_value::<
                    Vec<yuki_vtt_rules::changes::Change>,
                >(data["changes"].clone())
                .is_ok(),
                Some(_) => true,
                None => false,
            }
        }
        _ => true,
    }
}

fn as_sql(value: &Value) -> Sql {
    match value {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map(Sql::Integer)
            .unwrap_or_else(|| Sql::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(text) => Sql::Text(text.clone()),
        other => Sql::Text(other.to_string()),
    }
}

fn read_rows(
    db: &Connection,
    part: &str,
    columns: &[&str],
    table_id: &str,
) -> rusqlite::Result<Vec<Value>> {
    let key = if part == "tables" { "id" } else { "table_id" };
    let order = if part == "log" { " ORDER BY seq" } else { "" };
    let sql = format!(
        "SELECT {} FROM {part} WHERE {key} = ?1{order}",
        columns.join(", ")
    );
    let mut statement = db.prepare(&sql)?;
    statement
        .query_map([table_id], |row| {
            let mut fields = Map::new();
            for (i, column) in columns.iter().enumerate() {
                fields.insert(column.to_string(), as_json(row.get_ref(i)?));
            }
            Ok(Value::Object(fields))
        })?
        .collect()
}

/// Writes one table to `out`. Works while the host is on.
pub fn export(data_dir: &Path, table_id: &str, out: &Path) -> Result<(), String> {
    let db = db::open(&data_dir.join("tables.db"))
        .map_err(|e| format!("Couldn't open the tables: {e}"))?;
    let mut rows = Map::new();
    for (part, columns) in PARTS {
        let found = read_rows(&db, part, columns, table_id)
            .map_err(|e| format!("Couldn't read the table: {e}"))?;
        rows.insert(part.to_string(), Value::Array(found));
    }
    if rows["tables"].as_array().is_none_or(Vec::is_empty) {
        return Err("That table isn't on this computer".into());
    }
    let manifest = json!({ "format": FORMAT, "app": "yuki-vtt-server", "version": env!("CARGO_PKG_VERSION"), "rows": rows });

    let file = std::fs::File::create(out).map_err(problem("Couldn't write the backup"))?;
    let mut archive = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    let text = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let mut header = tar::Header::new_gnu();
    header.set_size(text.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "table.json", text.as_slice())
        .map_err(problem("Couldn't write the backup"))?;
    let pictures = data_dir.join("pictures").join(table_id);
    if pictures.is_dir() {
        archive
            .append_dir_all("pictures", &pictures)
            .map_err(problem("Couldn't add the pictures"))?;
    }
    archive
        .into_inner()
        .and_then(|gz| gz.finish())
        .map_err(problem("Couldn't finish the backup"))?;
    Ok(())
}

// Picture files are named by the host: letters, digits and dashes, then .map or .pic.
fn safe_picture(name: &str) -> bool {
    let Some((stem, kind)) = name.rsplit_once('.') else {
        return false;
    };
    (kind == "map" || kind == "pic")
        && !stem.is_empty()
        && stem.len() <= 40
        && stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Brings a backed-up table onto this computer and says which it was. A table already here
/// with the same id is kept unless `replace` says to put the backup in its place.
pub fn restore(data_dir: &Path, file: &Path, replace: bool) -> Result<(String, String), String> {
    let size = std::fs::metadata(file)
        .map_err(problem("Couldn't open the backup"))?
        .len();
    if size > BIGGEST {
        return Err("That file is too big to be a table".into());
    }
    let reader = std::fs::File::open(file).map_err(problem("Couldn't open the backup"))?;
    let mut archive = tar::Archive::new(GzDecoder::new(reader));
    let mut manifest: Option<Value> = None;
    let mut pictures: Vec<(String, Vec<u8>)> = Vec::new();
    let not_ours = |_| "That isn't a table backup".to_string();
    // A small file can unpack into a huge one: nothing is read past what a table can hold.
    let mut unpacked: u64 = 0;
    for entry in archive.entries().map_err(not_ours)? {
        let mut entry = entry.map_err(not_ours)?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(not_ours)?
            .to_string_lossy()
            .to_string();
        let limit = if path == "table.json" {
            TABLE_LIMIT
        } else {
            crate::pictures::MAP_LIMIT as u64
        };
        let mut bytes = Vec::new();
        (&mut entry)
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(not_ours)?;
        unpacked += bytes.len() as u64;
        if bytes.len() as u64 > limit || unpacked > BIGGEST {
            return Err("That backup unpacks into more than a table can hold".into());
        }
        if path == "table.json" {
            manifest = Some(
                serde_json::from_slice(&bytes)
                    .map_err(|_| "The backup's table is damaged".to_string())?,
            );
        } else if let Some(name) = path
            .strip_prefix("pictures/")
            .filter(|name| safe_picture(name))
        {
            pictures.push((name.to_string(), bytes));
        }
    }
    let manifest = manifest.ok_or("That isn't a table backup")?;
    if manifest["format"].as_u64() != Some(FORMAT) {
        return Err("That backup is from a newer Yuki VTT Server: update this one first".into());
    }
    let rows = &manifest["rows"];
    let table = rows["tables"]
        .get(0)
        .ok_or("The backup has no table in it")?;
    let id = table["id"]
        .as_str()
        .filter(|id| safe_picture(&format!("{id}.map")))
        .ok_or("The backup's table has no proper id")?
        .to_string();
    let name = table["name"].as_str().unwrap_or("Table").to_string();

    let mut db = db::open(&data_dir.join("tables.db"))
        .map_err(|e| format!("Couldn't open the tables: {e}"))?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let here: i64 = tx
        .query_row("SELECT COUNT(*) FROM tables WHERE id = ?1", [&id], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if here > 0 && !replace {
        return Err(format!("{name} is already on this computer"));
    }
    tx.execute("DELETE FROM tables WHERE id = ?1", [&id])
        .map_err(|e| e.to_string())?;
    for (part, columns) in PARTS {
        for row in rows[part].as_array().into_iter().flatten() {
            // Every row belongs to this table, whatever the file says.
            let owner = if part == "tables" {
                &row["id"]
            } else {
                &row["table_id"]
            };
            if owner.as_str() != Some(id.as_str()) {
                continue;
            }
            // Only the columns the row has: an older backup lacks newer ones, which keep their
            // defaults.
            let present: Vec<&str> = columns
                .iter()
                .copied()
                .filter(|column| row.get(*column).is_some())
                .collect();
            if !present.iter().all(|column| fits(column, &row[*column])) || !row_fits(part, row) {
                return Err(format!("The backup's {part} are damaged"));
            }
            let values: Vec<Sql> = present.iter().map(|column| as_sql(&row[*column])).collect();
            let marks = (1..=present.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "INSERT INTO {part} ({}) VALUES ({marks})",
                present.join(", ")
            );
            tx.execute(&sql, params_from_iter(values))
                .map_err(|e| format!("The backup's {part} didn't fit: {e}"))?;
        }
    }
    // Explored places only for this table's own maps.
    tx.execute(
        "DELETE FROM explored WHERE table_id = ?1
         AND scene_id NOT IN (SELECT id FROM scenes WHERE table_id = ?1)",
        [&id],
    )
    .map_err(|e| e.to_string())?;
    let folder = data_dir.join("pictures").join(&id);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).map_err(problem("Couldn't make the pictures folder"))?;
    for (file, bytes) in pictures {
        std::fs::write(folder.join(file), bytes).map_err(problem("Couldn't save a picture"))?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok((id, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_picture_names_inside_the_table() {
        assert!(safe_picture("tok-Ab1.pic"));
        assert!(safe_picture("Ab1.map"));
        assert!(!safe_picture("../../etc/passwd"));
        assert!(!safe_picture("a/b.pic"));
        assert!(!safe_picture("evil.sh"));
    }
}
