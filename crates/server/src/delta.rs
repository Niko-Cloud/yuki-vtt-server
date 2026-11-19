//! What changed between two copies of the table, so a screen gets a few hundred bytes after a
//! move instead of the whole table over the DM's upload. The same deltas as the website's
//! (`packages/engine/src/delta.ts`), so one browser applies both.
//!
//! A delta is one of `{ "v": value }` (the value, whole), `{ "o": { key: delta }, "x": [keys] }`
//! (an object: these keys changed, these went) or `{ "a": { "set": { id: delta }, "order":
//! [ids] } }` (a list of things with ids: these changed or are new; the new order when it
//! moved, or `"drop": n, "add": [ids]` when the list only lost its first few and gained some at
//! the end, as the log does). No delta means nothing changed.

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

// A list whose every item is an object with its own string id: changes go by id, not by place,
// so a new log line doesn't rewrite every line after it.
fn keyed(list: &[Value]) -> Option<Vec<&str>> {
    let ids: Option<Vec<&str>> = list.iter().map(|item| item["id"].as_str()).collect();
    let ids = ids?;
    let unique: HashSet<&&str> = ids.iter().collect();
    (unique.len() == ids.len()).then_some(ids)
}

// The new order: the short form when the list only slid along, as the log does.
fn reorder(from_ids: &[&str], to_ids: &[&str], list: &mut Map<String, Value>) {
    let had: HashSet<&str> = from_ids.iter().copied().collect();
    let stays = to_ids
        .iter()
        .position(|id| !had.contains(id))
        .unwrap_or(to_ids.len());
    let slid = from_ids.len() >= stays
        && to_ids[stays..].iter().all(|id| !had.contains(id))
        && to_ids[..stays] == from_ids[from_ids.len() - stays..];
    if slid {
        list.insert("drop".into(), json!(from_ids.len() - stays));
        list.insert("add".into(), json!(to_ids[stays..]));
    } else {
        list.insert("order".into(), json!(to_ids));
    }
}

/// What turns `from` into `to`, or `None` when they are the same.
pub fn delta(from: &Value, to: &Value) -> Option<Value> {
    if from == to {
        return None;
    }
    match (from, to) {
        (Value::Object(from), Value::Object(to)) => {
            let mut changed = Map::new();
            for (key, value) in to {
                let d = match from.get(key) {
                    Some(before) => delta(before, value),
                    None => Some(json!({ "v": value })),
                };
                if let Some(d) = d {
                    changed.insert(key.clone(), d);
                }
            }
            let gone: Vec<&String> = from.keys().filter(|key| !to.contains_key(*key)).collect();
            if gone.is_empty() {
                Some(json!({ "o": changed }))
            } else {
                Some(json!({ "o": changed, "x": gone }))
            }
        }
        (Value::Array(from_list), Value::Array(to_list)) => {
            match (keyed(from_list), keyed(to_list)) {
                (Some(from_ids), Some(to_ids)) => {
                    let before: HashMap<&str, &Value> =
                        from_ids.iter().copied().zip(from_list).collect();
                    let mut set = Map::new();
                    for (id, item) in to_ids.iter().zip(to_list) {
                        let d = match before.get(id) {
                            Some(was) => delta(was, item),
                            None => Some(json!({ "v": item })),
                        };
                        if let Some(d) = d {
                            set.insert(id.to_string(), d);
                        }
                    }
                    let mut list = Map::new();
                    if !set.is_empty() {
                        list.insert("set".into(), Value::Object(set));
                    }
                    if from_ids != to_ids {
                        reorder(&from_ids, &to_ids, &mut list);
                    }
                    Some(json!({ "a": list }))
                }
                _ => Some(json!({ "v": to })),
            }
        }
        _ => Some(json!({ "v": to })),
    }
}

/// The copy a delta makes of `from`, as the browser makes it.
pub fn apply(from: &Value, delta: Option<&Value>) -> Value {
    let Some(delta) = delta else {
        return from.clone();
    };
    if let Some(value) = delta.get("v") {
        return value.clone();
    }
    if let Some(changed) = delta.get("o").and_then(Value::as_object) {
        let mut next = from.as_object().cloned().unwrap_or_default();
        for (key, d) in changed {
            let was = next.get(key).cloned().unwrap_or(Value::Null);
            next.insert(key.clone(), apply(&was, Some(d)));
        }
        for key in delta["x"].as_array().into_iter().flatten() {
            if let Some(key) = key.as_str() {
                next.remove(key);
            }
        }
        return Value::Object(next);
    }
    let list = from.as_array().cloned().unwrap_or_default();
    let by_id: HashMap<String, Value> = list
        .iter()
        .filter_map(|item| Some((item["id"].as_str()?.to_string(), item.clone())))
        .collect();
    let ids: Vec<String> = list
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect();
    let strings = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|id| id.as_str().map(str::to_string))
            .collect()
    };
    let order: Vec<String> = if delta["a"].get("order").is_some() {
        strings(&delta["a"]["order"])
    } else if delta["a"].get("drop").is_some() || delta["a"].get("add").is_some() {
        let drop = delta["a"]["drop"].as_u64().unwrap_or(0) as usize;
        let mut order: Vec<String> = ids.into_iter().skip(drop).collect();
        order.extend(strings(&delta["a"]["add"]));
        order
    } else {
        ids
    };
    Value::Array(
        order
            .iter()
            .map(|id| {
                let was = by_id.get(id).cloned().unwrap_or(Value::Null);
                apply(&was, delta["a"]["set"].get(id))
            })
            .collect(),
    )
}

// A few recent copies per viewer; a screen whose copy is gone gets the whole table.
const KEPT_PER_VIEWER: usize = 3;
const MOST_VIEWERS: usize = 300;
// A delta nearly as big as the table isn't worth it: the whole table goes instead.
const WORTH_IT: f64 = 0.6;

// A viewer's recent copies, newest first: (version, table).
type Copies = Vec<(String, Value)>;

#[derive(Clone, Default)]
pub struct ViewVersions {
    // Kept in the order they were last used, so the quietest viewer goes first when full.
    copies: Arc<Mutex<VecDeque<(String, Copies)>>>,
}

impl ViewVersions {
    /// The table as this viewer should get it: whole with its version, or, when they already
    /// have `base`, only what changed (`delta` left out when nothing did).
    pub fn answer(&self, viewer: &str, base: Option<&str>, view: Value) -> Value {
        let text = view.to_string();
        let version: String = hex::encode(Sha256::digest(text.as_bytes()))
            .chars()
            .take(16)
            .collect();
        let had = {
            let mut copies = self.copies.lock().unwrap_or_else(|p| p.into_inner());
            let at = copies.iter().position(|(who, _)| who == viewer);
            let mut kept = at
                .and_then(|at| copies.remove(at))
                .map(|(_, kept)| kept)
                .unwrap_or_default();
            let had = base.and_then(|base| {
                kept.iter()
                    .find(|(version, _)| version == base)
                    .map(|(_, view)| view.clone())
            });
            kept.retain(|(v, _)| *v != version);
            kept.insert(0, (version.clone(), view.clone()));
            kept.truncate(KEPT_PER_VIEWER);
            copies.push_back((viewer.to_string(), kept));
            if copies.len() > MOST_VIEWERS {
                copies.pop_front();
            }
            had
        };
        if let (Some(had), Some(base)) = (had, base) {
            let d = delta(&had, &view);
            let size = d.as_ref().map(|d| d.to_string().len()).unwrap_or(0);
            if (size as f64) < text.len() as f64 * WORTH_IT {
                let mut answer = json!({ "version": version, "base": base });
                if let Some(d) = d {
                    answer["delta"] = d;
                }
                return answer;
            }
        }
        let mut whole = view;
        whole["version"] = json!(version);
        whole
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Value {
        json!({
            "party": { "id": "p1", "name": "Tuesday group" },
            "scene": {
                "tokens": [
                    { "id": "t1", "name": "Tam", "x": 1, "y": 2 },
                    { "id": "t2", "name": "Ogre", "x": 4, "y": 4, "hidden": true }
                ],
                "walls": [{ "id": "w1", "a": [0, 0], "b": [70, 0] }]
            },
            "log": [
                { "id": "l1", "data": { "label": "Attack", "total": 14 } },
                { "id": "l2", "data": { "label": "Damage", "total": 7 } }
            ],
            "encounter": null
        })
    }

    fn round_trip(from: &Value, to: &Value) -> Option<Value> {
        let d = delta(from, to);
        assert_eq!(&apply(from, d.as_ref()), to);
        d
    }

    #[test]
    fn says_nothing_when_nothing_changed() {
        assert_eq!(delta(&table(), &table()), None);
    }

    #[test]
    fn sends_a_moved_token_as_just_its_new_place() {
        let mut moved = table();
        moved["scene"]["tokens"][0]["x"] = json!(5);
        assert_eq!(
            round_trip(&table(), &moved),
            Some(
                json!({ "o": { "scene": { "o": { "tokens": { "a": { "set": { "t1": { "o": { "x": { "v": 5 } } } } } } } } } })
            )
        );
    }

    #[test]
    fn sends_a_new_log_line_and_the_new_order_like_the_website() {
        let mut next = table();
        next["log"] = json!([
            { "id": "l2", "data": { "label": "Damage", "total": 7 } },
            { "id": "l3", "data": { "label": "Heal", "total": 9 } }
        ]);
        assert_eq!(
            round_trip(&table(), &next),
            Some(json!({ "o": { "log": { "a": {
                "set": { "l3": { "v": { "id": "l3", "data": { "label": "Heal", "total": 9 } } } },
                "drop": 1,
                "add": ["l3"]
            } } } }))
        );
    }

    #[test]
    fn sends_a_new_order_whole_when_things_moved_about() {
        let mut swapped = table();
        swapped["scene"]["tokens"].as_array_mut().unwrap().reverse();
        assert_eq!(
            round_trip(&table(), &swapped),
            Some(
                json!({ "o": { "scene": { "o": { "tokens": { "a": { "order": ["t2", "t1"] } } } } } })
            )
        );
        let list = json!({ "list": [{ "id": "a" }, { "id": "b" }, { "id": "c" }] });
        round_trip(
            &list,
            &json!({ "list": [{ "id": "a" }, { "id": "c" }, { "id": "d" }] }),
        );
        round_trip(&list, &json!({ "list": [{ "id": "b" }, { "id": "c" }] }));
        round_trip(&list, &json!({ "list": [{ "id": "x" }] }));
    }

    #[test]
    fn takes_away_keys_and_sends_other_lists_whole() {
        let mut next = table();
        next["scene"]["tokens"][1]
            .as_object_mut()
            .unwrap()
            .remove("hidden");
        next["scene"]["walls"] = json!([]);
        next["encounter"] = json!({ "round": 1 });
        round_trip(&table(), &next);
        round_trip(
            &json!({ "p": [[0, 0], [1, 1]] }),
            &json!({ "p": [[0, 0], [2, 2]] }),
        );
        round_trip(
            &json!([{ "id": "a" }, { "id": "a" }]),
            &json!([{ "id": "a" }]),
        );
        round_trip(&json!({ "a": null }), &json!({ "a": [{ "id": "x" }] }));
    }

    #[test]
    fn answers_with_what_changed_since_the_copy_a_viewer_has() {
        let versions = ViewVersions::default();
        let first = versions.answer("dm", None, table());
        let version = first["version"].as_str().unwrap().to_string();
        let mut moved = table();
        moved["scene"]["tokens"][0]["x"] = json!(5);
        let answer = versions.answer("dm", Some(&version), moved.clone());
        assert_eq!(answer["base"], version.as_str());
        let mut had = first.clone();
        had.as_object_mut().unwrap().remove("version");
        assert_eq!(apply(&had, answer.get("delta")), moved);
        // Another viewer's version, or a lost one, gets the whole table.
        assert!(
            versions
                .answer("player", Some(&version), moved.clone())
                .get("party")
                .is_some()
        );
        assert!(
            versions
                .answer("dm", Some("gone"), moved)
                .get("party")
                .is_some()
        );
    }
}
