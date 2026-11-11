//! A seated character, as the website worked it out: the character, its sheet and what the
//! table shows of it. The host keeps the sheet as it came and only reads the few things the
//! table needs (the name, AC, the initiative bonus, the hit point maximum), plus the play
//! state the DM changes during the game.

use crate::error::{AppError, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::Sha256;
use yuki_vtt_rules::changes::Fact;
use yuki_vtt_rules::hit_points;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub name: String,
    pub classes: String,
    pub armor_class: Option<i64>,
    pub initiative: Option<i64>,
    pub max_hit_points: Option<i64>,
    pub edition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Who plays it, as the website tells players apart; bans go by this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_id: Option<String>,
    /// A character saved on the website's account; none for one kept in the browser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
    pub summary: Summary,
    pub character: Map<String, Value>,
    #[serde(default)]
    pub sheet: Value,
    #[serde(default)]
    pub play: Value,
    #[serde(default)]
    pub companions: Value,
    /// The character as the DM view lists it (level, passive scores, spell slots and more),
    /// worked out by the website.
    #[serde(default)]
    pub member: Value,
    /// Its numbers and gear, as the website lists them, to tell the DM what a player changed.
    #[serde(default)]
    pub facts: Vec<Fact>,
}

/// What a browser sends to sit down or to update its character: the snapshot as the website
/// wrote it, and the website's signature of exactly that text.
#[derive(Debug, Deserialize)]
pub struct Signed {
    pub snapshot: String,
    #[serde(default)]
    pub signature: String,
}

const BIGGEST: usize = 1024 * 1024;

impl Signed {
    /// The snapshot, once the signature shows the website made it.
    pub fn open(&self, secret: Option<&[u8]>) -> Result<Snapshot> {
        if self.snapshot.len() > BIGGEST {
            return Err(AppError::bad("That character is too big for the table"));
        }
        if let Some(secret) = secret {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("any key length works");
            mac.update(self.snapshot.as_bytes());
            let given = hex::decode(&self.signature).unwrap_or_default();
            mac.verify_slice(&given).map_err(|_| {
                AppError::forbidden(
                    "The website didn't sign this character: open it from Yuki Char Maker",
                )
            })?;
        }
        let snapshot: Snapshot = serde_json::from_str(&self.snapshot)
            .map_err(|_| AppError::bad("That isn't a character the table can read"))?;
        let name = snapshot.summary.name.trim();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(AppError::bad(
                "A character needs a name of up to 100 letters",
            ));
        }
        if !snapshot
            .character
            .get("state")
            .is_some_and(Value::is_object)
        {
            return Err(AppError::bad("That character has no play state"));
        }
        Ok(snapshot)
    }
}

impl Snapshot {
    pub fn state(&self) -> Map<String, Value> {
        self.character
            .get("state")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_state(&mut self, state: Map<String, Value>) {
        self.character.insert("state".into(), Value::Object(state));
    }

    pub fn rules_sheet(&self) -> Option<hit_points::Sheet> {
        self.summary.max_hit_points.map(|max| hit_points::Sheet {
            max,
            edition_2024: self.summary.edition == "2024",
        })
    }

    /// Hit points as the table shows them, worked out from the play state as it is now.
    pub fn hit_points(&self) -> Value {
        let Some(sheet) = self.rules_sheet() else {
            return Value::Null;
        };
        let state = self.state();
        json!({
            "current": hit_points::current(&state, sheet),
            "max": sheet.max,
            "temporary": state.get("temporaryHitPoints").and_then(Value::as_i64).unwrap_or(0),
        })
    }

    /// The seated character as everyone at the table sees them.
    pub fn member(&self, seat_id: &str, permissions: &Value) -> Value {
        let state = self.state();
        json!({
            "seatId": seat_id,
            "permissions": permissions,
            "name": self.summary.name,
            "classes": self.summary.classes,
            "hitPoints": self.hit_points(),
            "armorClass": self.summary.armor_class,
            "conditions": state.get("conditions").cloned().unwrap_or_else(|| json!([])),
            "exhaustion": state.get("exhaustion").and_then(Value::as_i64).unwrap_or(0),
            "concentrating": state.get("concentrating").and_then(Value::as_bool).unwrap_or(false),
            "deathSaves": state.get("deathSaves").cloned().unwrap_or_else(|| json!({ "successes": 0, "failures": 0 })),
        })
    }

    /// The sheet for the character panel. The website worked out `play` when the player
    /// last saved, so its hit points are brought up to date with the DM's changes since.
    pub fn sheet_view(&self) -> Value {
        let mut play = self.play.clone();
        if let (Some(play), Value::Object(hp)) = (play.as_object_mut(), self.hit_points()) {
            play.insert("hitPoints".into(), Value::Object(hp));
        }
        json!({
            "character": self.character,
            "sheet": self.sheet,
            "play": play,
            "companions": if self.companions.is_null() { json!([]) } else { self.companions.clone() },
            "missing": [],
            "sharedBy": Value::Null,
        })
    }

    /// Brings the facts the DM changes at the table (hit points, conditions) up to date with
    /// the play state, in the website's words.
    pub fn refresh_table_facts(&mut self) {
        let state = self.state();
        let hp = self.hit_points();
        let mut set = |key: &str, value: String| {
            if let Some(fact) = self.facts.iter_mut().find(|f| f.key == key) {
                fact.value = value;
            }
        };
        if let (Some(current), Some(max)) = (hp["current"].as_i64(), hp["max"].as_i64()) {
            set("hp", format!("{current}/{max}"));
        }
        if let Some(temporary) = hp["temporary"].as_i64() {
            set("temp", temporary.to_string());
        }
        let mut conditions: Vec<&str> = state
            .get("conditions")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        conditions.sort_unstable();
        set(
            "conditions",
            if conditions.is_empty() {
                "none".into()
            } else {
                conditions.join(", ")
            },
        );
        set(
            "exhaustion",
            state
                .get("exhaustion")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .to_string(),
        );
        let concentrating = state
            .get("concentrating")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        set(
            "concentrating",
            if concentrating { "yes" } else { "no" }.into(),
        );
    }

    /// The character for the DM view: the website's summary, with what the DM may have
    /// changed since (hit points, conditions) as it is now.
    pub fn dm_member(&self) -> Value {
        let state = self.state();
        let mut member = match &self.member {
            Value::Object(member) => member.clone(),
            // An older website sent only the short summary.
            _ => json!({
                "name": self.summary.name,
                "edition": self.summary.edition,
                "level": 0,
                "classes": self.summary.classes,
                "armorClass": self.summary.armor_class,
                "initiative": self.summary.initiative,
                "speed": Value::Null,
                "passive": Value::Null,
                "spellSaveDcs": [],
                "spellSlots": [],
                "pactSlots": Value::Null,
                "hitDiceLeft": 0,
                "hitDiceTotal": 0,
                "inspiration": false,
                "missing": 0,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        };
        member.insert("hitPoints".into(), self.hit_points());
        member.insert(
            "conditions".into(),
            state
                .get("conditions")
                .cloned()
                .unwrap_or_else(|| json!([])),
        );
        member.insert(
            "exhaustion".into(),
            json!(state.get("exhaustion").and_then(Value::as_i64).unwrap_or(0)),
        );
        member.insert(
            "concentrating".into(),
            json!(
                state
                    .get("concentrating")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            ),
        );
        member.insert(
            "deathSaves".into(),
            state
                .get("deathSaves")
                .cloned()
                .unwrap_or_else(|| json!({ "successes": 0, "failures": 0 })),
        );
        Value::Object(member)
    }
}

/// The player saved while the DM changed something: both are kept. For each part of the play
/// state, whoever changed it since `base` wins, and the DM wins when both did.
pub fn merge(
    base: &Map<String, Value>,
    player: &Map<String, Value>,
    table: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = player.clone();
    for (key, value) in table {
        if base.get(key) != Some(value) {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn keeps_both_sides_and_lets_the_dm_win_a_clash() {
        let base = object(json!({ "currentHitPoints": 20, "spellSlotsLeft": [3], "notes": "a" }));
        let player = object(json!({ "currentHitPoints": 18, "spellSlotsLeft": [2], "notes": "a" }));
        let table = object(json!({ "currentHitPoints": 12, "spellSlotsLeft": [3], "notes": "a" }));
        let merged = merge(&base, &player, &table);
        assert_eq!(merged["currentHitPoints"], 12);
        assert_eq!(merged["spellSlotsLeft"], json!([2]));
    }

    #[test]
    fn refuses_a_snapshot_the_website_did_not_sign() {
        let text = json!({ "summary": { "name": "Tam", "classes": "Fighter 3", "armorClass": 18, "initiative": 2, "maxHitPoints": 28, "edition": "2014" }, "character": { "name": "Tam", "state": {} } }).to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
        mac.update(text.as_bytes());
        let signed = Signed {
            snapshot: text.clone(),
            signature: hex::encode(mac.finalize().into_bytes()),
        };
        assert_eq!(signed.open(Some(b"secret")).unwrap().summary.name, "Tam");
        let forged = Signed {
            snapshot: text.replace("18", "25"),
            signature: signed.signature.clone(),
        };
        assert_eq!(
            forged.open(Some(b"secret")).unwrap_err().status,
            axum::http::StatusCode::FORBIDDEN
        );
    }
}
