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
    /// The DM's fighters: hit points and armour class. Players get a word or nothing instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hp: Option<Hp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac: Option<i32>,
    /// How hurt it looks, for players, when the DM shows a word instead of numbers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hp {
    pub current: i32,
    pub max: i32,
}

/// A condition or spell on someone in the fight, for so many rounds or until taken off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    pub id: String,
    pub name: String,
    /// Rounds left, counted down at the end of its bearer's turn; none lasts until taken off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rounds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Encounter {
    pub round: u32,
    pub turn: usize,
    pub combatants: Vec<Combatant>,
    /// What players see of monsters' hit points: hidden, word or exact; a word when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_health: Option<String>,
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
        show_health: None,
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
        ..encounter.clone()
    }
}

/// The next turn; after the last in line, a new round starts at the top. The effects on
/// whoever just acted count down a round, and those that run out come off.
pub fn next_turn(encounter: &Encounter) -> Encounter {
    if encounter.combatants.is_empty() {
        return encounter.clone();
    }
    let mut combatants = encounter.combatants.clone();
    if let Some(acting) = combatants.get_mut(encounter.turn) {
        acting.effects = acting
            .effects
            .iter()
            .filter_map(|e| match e.rounds {
                None => Some(e.clone()),
                Some(1) | Some(0) => None,
                Some(n) => Some(Effect {
                    rounds: Some(n - 1),
                    ..e.clone()
                }),
            })
            .collect();
    }
    let turn = encounter.turn + 1;
    if turn >= combatants.len() {
        Encounter {
            combatants,
            turn: 0,
            round: encounter.round + 1,
            ..encounter.clone()
        }
    } else {
        Encounter {
            combatants,
            turn,
            ..encounter.clone()
        }
    }
}

/// The effects that run out when the turn now going ends: (who, effect).
pub fn effects_ending(encounter: &Encounter) -> Vec<(String, String)> {
    encounter
        .combatants
        .get(encounter.turn)
        .map(|c| {
            c.effects
                .iter()
                .filter(|e| e.rounds == Some(1))
                .map(|e| (c.name.clone(), e.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// How hurt someone looks from outside: unhurt, hurt, bloodied (half or less) or down.
pub fn health_word(hp: Hp) -> &'static str {
    if hp.current <= 0 {
        "down"
    } else if hp.current >= hp.max {
        "unhurt"
    } else if hp.current * 2 <= hp.max {
        "bloodied"
    } else {
        "hurt"
    }
}

/// Rolls initiative again: for the DM's fighters only, or for everyone.
pub fn reroll(encounter: &Encounter, everyone: bool, mut d20: impl FnMut() -> i32) -> Encounter {
    let combatants = encounter
        .combatants
        .iter()
        .map(|c| {
            if everyone || c.seat_id.is_none() {
                Combatant {
                    initiative: d20() + c.bonus,
                    ..c.clone()
                }
            } else {
                c.clone()
            }
        })
        .collect();
    reordered(encounter, combatants)
}

/// A change to someone in the fight. `Some(None)` takes hit points or armour class off.
#[derive(Debug, Default, Clone)]
pub struct Change {
    pub name: Option<String>,
    pub initiative: Option<i32>,
    pub hp: Option<Option<Hp>>,
    pub ac: Option<Option<i32>>,
    pub effects: Option<Vec<Effect>>,
}

pub fn change(encounter: &Encounter, id: &str, change: Change) -> Encounter {
    let combatants = encounter
        .combatants
        .iter()
        .map(|c| {
            if c.id != id {
                return c.clone();
            }
            let mut next = c.clone();
            if let Some(name) = &change.name {
                next.name = name.clone();
            }
            if let Some(initiative) = change.initiative {
                next.initiative = initiative;
            }
            if let Some(hp) = change.hp {
                next.hp = hp.map(|hp| Hp {
                    current: hp.current.min(hp.max),
                    ..hp
                });
            }
            if let Some(ac) = change.ac {
                next.ac = ac;
            }
            if let Some(effects) = &change.effects {
                next.effects = effects.clone();
            }
            next
        })
        .collect();
    reordered(encounter, combatants)
}

/// The fight as players see it: no armour class for the DM's fighters, and their hit points as
/// the DM chose to show them (exact, a word like "bloodied", or not at all).
pub fn for_players(encounter: &Encounter) -> Encounter {
    let shown = encounter.show_health.as_deref().unwrap_or("word");
    let combatants = encounter
        .combatants
        .iter()
        .map(|c| {
            if c.seat_id.is_some() {
                return c.clone();
            }
            let mut seen = Combatant {
                ac: None,
                hp: None,
                ..c.clone()
            };
            match (c.hp, shown) {
                (Some(hp), "exact") => seen.hp = Some(hp),
                (Some(hp), "word") => seen.health = Some(health_word(hp).into()),
                _ => {}
            }
            seen
        })
        .collect();
    Encounter {
        combatants,
        ..encounter.clone()
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
            hp: None,
            ac: None,
            health: None,
            effects: vec![],
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

    #[test]
    fn counts_effects_down_at_the_end_of_their_bearers_turn() {
        let fight = start(vec![fighter("A", 3), fighter("B", 1)], || 10);
        let effect = |id: &str, rounds| Effect {
            id: id.into(),
            name: id.into(),
            rounds,
        };
        let fight = change(
            &fight,
            "A",
            Change {
                effects: Some(vec![
                    effect("Bless", Some(2)),
                    effect("Poisoned", Some(1)),
                    effect("Prone", None),
                ]),
                ..Change::default()
            },
        );
        assert_eq!(
            effects_ending(&fight),
            [("A".to_string(), "Poisoned".to_string())]
        );
        let fight = next_turn(&fight);
        assert_eq!(
            fight.combatants[0].effects,
            [effect("Bless", Some(1)), effect("Prone", None)]
        );
        // B's turn ending leaves A's effects alone.
        let fight = next_turn(&fight);
        assert_eq!(fight.combatants[0].effects.len(), 2);
        let fight = next_turn(&fight);
        assert_eq!(fight.combatants[0].effects, [effect("Prone", None)]);
    }

    #[test]
    fn shows_players_a_word_numbers_or_nothing() {
        let words: Vec<_> = [0, 3, 6, 7, 12]
            .into_iter()
            .map(|current| health_word(Hp { current, max: 12 }))
            .collect();
        assert_eq!(words, ["down", "bloodied", "bloodied", "hurt", "unhurt"]);
        let fight = change(
            &start(vec![fighter("Ogre", 0)], || 10),
            "Ogre",
            Change {
                hp: Some(Some(Hp {
                    current: 40,
                    max: 12,
                })),
                ac: Some(Some(11)),
                ..Change::default()
            },
        );
        assert_eq!(
            fight.combatants[0].hp,
            Some(Hp {
                current: 12,
                max: 12
            })
        );
        let seen = for_players(&fight);
        assert_eq!((seen.combatants[0].hp, seen.combatants[0].ac), (None, None));
        assert_eq!(seen.combatants[0].health.as_deref(), Some("unhurt"));
        let exact = for_players(&Encounter {
            show_health: Some("exact".into()),
            ..fight.clone()
        });
        assert_eq!(
            exact.combatants[0].hp,
            Some(Hp {
                current: 12,
                max: 12
            })
        );
        let rolled = reroll(&fight, false, || 20);
        assert_eq!(rolled.combatants[0].initiative, 20);
    }
}
