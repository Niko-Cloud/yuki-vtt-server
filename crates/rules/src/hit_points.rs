//! What the DM may change on a seated character from the table: damage, healing, temporary
//! hit points and conditions. The character's play state is kept as the website sent it; only
//! the fields these changes touch are read or written, so nothing else is lost on the way back.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TableChange {
    Damage {
        amount: u32,
    },
    Heal {
        amount: u32,
    },
    Temp {
        amount: u32,
    },
    Conditions {
        conditions: Vec<String>,
        exhaustion: u32,
    },
}

/// What the rules need from the sheet: the hit point maximum, and the edition (2024 caps the
/// concentration save at 30).
#[derive(Debug, Clone, Copy)]
pub struct Sheet {
    pub max: i64,
    pub edition_2024: bool,
}

fn number(state: &Map<String, Value>, key: &str) -> Option<i64> {
    state.get(key).and_then(Value::as_i64)
}

/// Hit points now: a missing or empty `currentHitPoints` means full.
pub fn current(state: &Map<String, Value>, sheet: Sheet) -> i64 {
    number(state, "currentHitPoints")
        .unwrap_or(sheet.max)
        .min(sheet.max)
}

fn temporary(state: &Map<String, Value>) -> i64 {
    number(state, "temporaryHitPoints").unwrap_or(0)
}

fn with_hit_points(
    mut state: Map<String, Value>,
    sheet: Sheet,
    current: i64,
) -> Map<String, Value> {
    let stored = if current >= sheet.max {
        Value::Null
    } else {
        json!(current.max(0))
    };
    state.insert("currentHitPoints".into(), stored);
    // Back above 0 by any means: the death save tally starts over.
    if current > 0 {
        state.insert(
            "deathSaves".into(),
            json!({ "successes": 0, "failures": 0 }),
        );
    }
    state
}

/// Applies a change from the DM to a character's play state.
pub fn apply(state: &Map<String, Value>, sheet: Sheet, change: &TableChange) -> Map<String, Value> {
    match change {
        TableChange::Damage { amount } => take_damage(state, sheet, i64::from(*amount)),
        TableChange::Heal { amount } => with_hit_points(
            state.clone(),
            sheet,
            current(state, sheet) + i64::from(*amount),
        ),
        // Temporary hit points don't stack: you keep whichever is higher.
        TableChange::Temp { amount } => {
            let mut next = state.clone();
            next.insert(
                "temporaryHitPoints".into(),
                json!(temporary(state).max(i64::from(*amount))),
            );
            next
        }
        TableChange::Conditions {
            conditions,
            exhaustion,
        } => {
            let mut next = state.clone();
            next.insert("conditions".into(), json!(conditions));
            next.insert("exhaustion".into(), json!(exhaustion));
            next
        }
    }
}

// Temporary hit points go first. At 0 concentration ends; otherwise a concentrating
// character owes a save, DC 10 or half the damage.
fn take_damage(state: &Map<String, Value>, sheet: Sheet, amount: i64) -> Map<String, Value> {
    if amount <= 0 {
        return state.clone();
    }
    let absorbed = temporary(state).min(amount);
    let mut next = state.clone();
    next.insert(
        "temporaryHitPoints".into(),
        json!(temporary(state) - absorbed),
    );
    let mut next = with_hit_points(next, sheet, current(state, sheet) - (amount - absorbed));
    if current(&next, sheet) == 0 {
        next.insert("concentrating".into(), json!(false));
        next.insert("concentrationChecks".into(), json!([]));
        next.remove("pendingConcentrationRoll");
    } else if state.get("concentrating").and_then(Value::as_bool) == Some(true) {
        let dc = (amount / 2).max(10);
        let dc = if sheet.edition_2024 { dc.min(30) } else { dc };
        let mut checks = state
            .get("concentrationChecks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        checks.push(json!(dc));
        next.insert("concentrationChecks".into(), Value::Array(checks));
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHEET: Sheet = Sheet {
        max: 30,
        edition_2024: false,
    };

    fn state(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn takes_temporary_hit_points_first() {
        let start =
            state(json!({ "currentHitPoints": null, "temporaryHitPoints": 5, "notes": "kept" }));
        let hit = apply(&start, SHEET, &TableChange::Damage { amount: 8 });
        assert_eq!(hit["temporaryHitPoints"], 0);
        assert_eq!(hit["currentHitPoints"], 27);
        assert_eq!(hit["notes"], "kept");
    }

    #[test]
    fn stops_at_zero_and_ends_concentration() {
        let start = state(
            json!({ "currentHitPoints": 4, "temporaryHitPoints": 0, "concentrating": true, "pendingConcentrationRoll": 10 }),
        );
        let down = apply(&start, SHEET, &TableChange::Damage { amount: 50 });
        assert_eq!(down["currentHitPoints"], 0);
        assert_eq!(down["concentrating"], false);
        assert!(!down.contains_key("pendingConcentrationRoll"));
    }

    #[test]
    fn asks_a_concentrating_caster_for_a_save() {
        let start = state(
            json!({ "currentHitPoints": 30, "temporaryHitPoints": 0, "concentrating": true }),
        );
        // Half of 80 is 40, but 2024 caps the save at 30.
        let tough = state(json!({ "currentHitPoints": 100, "concentrating": true }));
        let hit = apply(
            &tough,
            Sheet {
                max: 100,
                edition_2024: true,
            },
            &TableChange::Damage { amount: 80 },
        );
        assert_eq!(hit["concentrationChecks"], json!([30]));
        let small = apply(&start, SHEET, &TableChange::Damage { amount: 4 });
        assert_eq!(small["concentrationChecks"], json!([10]));
    }

    #[test]
    fn heals_up_to_full_and_resets_death_saves() {
        let start = state(
            json!({ "currentHitPoints": 0, "deathSaves": { "successes": 1, "failures": 2 } }),
        );
        let healed = apply(&start, SHEET, &TableChange::Heal { amount: 99 });
        assert_eq!(healed["currentHitPoints"], Value::Null);
        assert_eq!(
            healed["deathSaves"],
            json!({ "successes": 0, "failures": 0 })
        );
    }

    #[test]
    fn keeps_the_higher_temporary_hit_points() {
        let start = state(json!({ "temporaryHitPoints": 8 }));
        assert_eq!(
            apply(&start, SHEET, &TableChange::Temp { amount: 5 })["temporaryHitPoints"],
            8
        );
        assert_eq!(
            apply(&start, SHEET, &TableChange::Temp { amount: 12 })["temporaryHitPoints"],
            12
        );
    }

    #[test]
    fn reads_the_change_the_website_sends() {
        let change: TableChange = serde_json::from_value(
            json!({ "kind": "conditions", "conditions": ["prone"], "exhaustion": 1 }),
        )
        .unwrap();
        let next = apply(&Map::new(), SHEET, &change);
        assert_eq!(next["conditions"], json!(["prone"]));
        assert_eq!(next["exhaustion"], 1);
    }
}
