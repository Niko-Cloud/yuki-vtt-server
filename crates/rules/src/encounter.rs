//! A fight at the table: the initiative order, whose turn it is, and the round.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Combatant {
    pub id: String,
    pub name: String,
    pub initiative: i32,
    /// The initiative bonus breaks ties.
    pub bonus: i32,
    /// A seated character; monsters and NPCs the DM adds have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seat_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Encounter {
    pub round: u32,
    pub turn: usize,
    pub combatants: Vec<Combatant>,
}

// Highest first. Ties go to the higher bonus, then by name, so everyone sees the same order.
fn by_initiative(a: &Combatant, b: &Combatant) -> Ordering {
    b.initiative
        .cmp(&a.initiative)
        .then(b.bonus.cmp(&a.bonus))
        .then_with(|| a.name.cmp(&b.name))
}

/// Rolls initiative for everyone and starts at the top of round 1. `d20` gives 1 to 20.
pub fn start(fighters: Vec<Combatant>, mut d20: impl FnMut() -> i32) -> Encounter {
    let mut combatants: Vec<Combatant> = fighters
        .into_iter()
        .map(|f| Combatant {
            initiative: d20() + f.bonus,
            ..f
        })
        .collect();
    combatants.sort_by(by_initiative);
    Encounter {
        round: 1,
        turn: 0,
        combatants,
    }
}

// Reorders after a change, keeping the turn on whoever had it.
fn reordered(encounter: &Encounter, mut combatants: Vec<Combatant>) -> Encounter {
    let current = encounter
        .combatants
        .get(encounter.turn)
        .map(|c| c.id.clone());
    combatants.sort_by(by_initiative);
    let turn = current
        .and_then(|id| combatants.iter().position(|c| c.id == id))
        .unwrap_or(0);
    Encounter {
        combatants,
        turn,
        ..encounter.clone()
    }
}

pub fn add(encounter: &Encounter, combatant: Combatant) -> Encounter {
    let mut combatants = encounter.combatants.clone();
    combatants.push(combatant);
    reordered(encounter, combatants)
}

pub fn set_initiative(encounter: &Encounter, id: &str, initiative: i32) -> Encounter {
    let combatants = encounter
        .combatants
        .iter()
        .map(|c| {
            if c.id == id {
                Combatant {
                    initiative,
                    ..c.clone()
                }
            } else {
                c.clone()
            }
        })
        .collect();
    reordered(encounter, combatants)
}

/// Takes someone out of the fight. If it was their turn, the next in line goes.
pub fn remove(encounter: &Encounter, id: &str) -> Encounter {
    let Some(at) = encounter.combatants.iter().position(|c| c.id == id) else {
        return encounter.clone();
    };
    let mut combatants = encounter.combatants.clone();
    combatants.remove(at);
    let (mut turn, mut round) = (encounter.turn, encounter.round);
    if at < turn {
        turn -= 1;
    }
    if turn >= combatants.len() {
        turn = 0;
        if !combatants.is_empty() {
            round += 1;
        }
    }
    Encounter {
        round,
        turn,
        combatants,
    }
}

/// The next turn; after the last in line, a new round starts at the top.
pub fn next_turn(encounter: &Encounter) -> Encounter {
    if encounter.combatants.is_empty() {
        return encounter.clone();
    }
    let turn = encounter.turn + 1;
    if turn >= encounter.combatants.len() {
        Encounter {
            turn: 0,
            round: encounter.round + 1,
            ..encounter.clone()
        }
    } else {
        Encounter {
            turn,
            ..encounter.clone()
        }
    }
}

/// Back one turn, for a DM who went too fast; never before round 1.
pub fn previous_turn(encounter: &Encounter) -> Encounter {
    if encounter.turn > 0 {
        return Encounter {
            turn: encounter.turn - 1,
            ..encounter.clone()
        };
    }
    if encounter.round == 1 {
        return encounter.clone();
    }
    Encounter {
        turn: encounter.combatants.len().saturating_sub(1),
        round: encounter.round - 1,
        ..encounter.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fighter(id: &str, bonus: i32) -> Combatant {
        Combatant {
            id: id.into(),
            name: id.into(),
            initiative: 0,
            bonus,
            seat_id: None,
        }
    }

    fn names(encounter: &Encounter) -> Vec<&str> {
        encounter
            .combatants
            .iter()
            .map(|c| c.name.as_str())
            .collect()
    }

    #[test]
    fn orders_by_initiative_then_bonus_then_name() {
        let mut rolls = [10, 12, 8, 10].into_iter();
        let fight = start(
            vec![
                fighter("Tam", 2),
                fighter("Ogre", 0),
                fighter("Wren", 4),
                fighter("Bo", 2),
            ],
            || rolls.next().unwrap(),
        );
        // Tam 12, Ogre 12, Wren 12, Bo 12: bonus first, then name.
        assert_eq!(names(&fight), ["Wren", "Bo", "Tam", "Ogre"]);
        assert_eq!((fight.round, fight.turn), (1, 0));
    }

    #[test]
    fn goes_round_and_back() {
        let fight = start(vec![fighter("A", 3), fighter("B", 1)], || 10);
        let fight = next_turn(&next_turn(&fight));
        assert_eq!((fight.round, fight.turn), (2, 0));
        let fight = previous_turn(&fight);
        assert_eq!((fight.round, fight.turn), (1, 1));
        let fight = previous_turn(&previous_turn(&fight));
        assert_eq!((fight.round, fight.turn), (1, 0));
    }

    #[test]
    fn keeps_the_turn_on_whoever_had_it() {
        let fight = next_turn(&start(vec![fighter("A", 3), fighter("B", 1)], || 10));
        assert_eq!(fight.combatants[fight.turn].id, "B");
        let fight = add(
            &fight,
            Combatant {
                initiative: 30,
                ..fighter("C", 0)
            },
        );
        assert_eq!(names(&fight), ["C", "A", "B"]);
        assert_eq!(fight.combatants[fight.turn].id, "B");
        let fight = set_initiative(&fight, "B", 40);
        assert_eq!(fight.turn, 0);
    }

    #[test]
    fn passes_the_turn_when_its_owner_leaves() {
        let fight = next_turn(&start(vec![fighter("A", 3), fighter("B", 1)], || 10));
        let fight = remove(&fight, "B");
        assert_eq!((fight.round, fight.turn), (2, 0));
        assert_eq!(remove(&fight, "nobody"), fight);
    }
}
