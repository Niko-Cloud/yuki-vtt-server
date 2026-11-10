//! Dice rolls: "1d20+5", "2d6+3", "8d6", "1d8+1d6-1", and "4d6kh3" to keep the highest three.
//! Only dice, whole numbers, + and - are understood. Rolls take their random numbers from
//! outside, so tests can fix them and the server can use the system's secure random.

use serde::{Deserialize, Serialize};

/// The dice that can be picked from the tray.
pub const DICE: [u32; 7] = [4, 6, 8, 10, 12, 20, 100];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiceTerm {
    pub count: u32,
    pub die: u32,
    pub sign: i32,
    /// Only the highest this many dice count; the rest are dropped.
    pub keep: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dice {
    pub terms: Vec<DiceTerm>,
    pub modifier: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Advantage {
    Advantage,
    Disadvantage,
    #[default]
    Normal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolledDie {
    pub die: u32,
    pub sign: i32,
    pub results: Vec<u32>,
    pub dropped: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Roll {
    pub total: i32,
    pub dice: Vec<RolledDie>,
    pub modifier: i32,
    /// The kept d20 when the roll was one d20, for "natural 20" and "natural 1".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub natural: Option<u32>,
}

// Biggest numbers a roll may have, so nobody can ask the host for a million dice.
const MOST_DICE: u32 = 100;
const BIGGEST_DIE: u32 = 1000;
const BIGGEST_FLAT: i32 = 10_000;

/// The parts of a roll, or `None` when the text isn't one.
pub fn parse_dice(text: &str) -> Option<Dice> {
    let compact: String = text
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if compact.is_empty() {
        return None;
    }
    let mut dice = Dice {
        terms: Vec::new(),
        modifier: 0,
    };
    let mut rest = compact.as_str();
    let mut first = true;
    while !rest.is_empty() {
        // Every term after the first needs its sign; the first may have one.
        let sign = match rest.as_bytes()[0] {
            b'+' => {
                rest = &rest[1..];
                1
            }
            b'-' => {
                rest = &rest[1..];
                -1
            }
            _ if first => 1,
            _ => return None,
        };
        first = false;
        let end = rest.find(['+', '-']).unwrap_or(rest.len());
        let (term, after) = rest.split_at(end);
        rest = after;
        if term.is_empty() {
            return None;
        }
        match term.split_once('d') {
            None => {
                let flat: i32 = digits(term)?.try_into().ok()?;
                if flat > BIGGEST_FLAT {
                    return None;
                }
                dice.modifier += sign * flat;
            }
            Some((count, die_part)) => {
                let count = if count.is_empty() { 1 } else { digits(count)? };
                let (die, keep) = match die_part.split_once("kh") {
                    Some((die, keep)) => (digits(die)?, Some(digits(keep)?)),
                    None => (digits(die_part)?, None),
                };
                if !(1..=MOST_DICE).contains(&count) || !(2..=BIGGEST_DIE).contains(&die) {
                    return None;
                }
                if keep.is_some_and(|keep| keep < 1 || keep > count) {
                    return None;
                }
                dice.terms.push(DiceTerm {
                    count,
                    die,
                    sign,
                    keep,
                });
            }
        }
    }
    (!dice.terms.is_empty() || dice.modifier != 0).then_some(dice)
}

// A whole number made of digits only (no sign, no letters).
fn digits(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Rolls the dice. `die(n)` gives a number from 1 to n.
///
/// With advantage or disadvantage a single d20 is rolled twice, keeping the higher or the
/// lower. A critical hit rolls every die twice as many times.
pub fn roll(
    dice: &Dice,
    advantage: Advantage,
    critical: bool,
    mut die: impl FnMut(u32) -> u32,
) -> Roll {
    let single = dice.terms.len() == 1 && dice.terms[0].count == 1 && dice.terms[0].die == 20;
    let rolled: Vec<RolledDie> = dice
        .terms
        .iter()
        .map(|term| {
            let times = if critical { term.count * 2 } else { term.count };
            let mut results: Vec<u32> = (0..times).map(|_| die(term.die)).collect();
            let mut dropped = Vec::new();
            if let Some(keep) = term.keep {
                // Drop the lowest dice, one each, keeping the rest in the order they were rolled.
                let mut lowest = results.clone();
                lowest.sort_unstable();
                lowest.truncate((times - keep.min(times)) as usize);
                for n in &lowest {
                    let at = results.iter().position(|r| r == n).expect("a rolled die");
                    results.remove(at);
                }
                dropped = lowest;
            }
            if single && advantage != Advantage::Normal {
                let other = die(term.die);
                let kept = match advantage {
                    Advantage::Advantage => results[0].max(other),
                    _ => results[0].min(other),
                };
                dropped = vec![if kept == results[0] {
                    other
                } else {
                    results[0]
                }];
                results[0] = kept;
            }
            RolledDie {
                die: term.die,
                sign: term.sign,
                results,
                dropped,
            }
        })
        .collect();
    let total = rolled
        .iter()
        .map(|d| d.sign * d.results.iter().sum::<u32>() as i32)
        .sum::<i32>()
        + dice.modifier;
    Roll {
        total,
        natural: single.then(|| rolled[0].results[0]),
        dice: rolled,
        modifier: dice.modifier,
    }
}

/// The dice as rolled, for the table log: "[14, 7] + 5", dropped dice after a slash.
pub fn roll_detail(result: &Roll) -> String {
    let mut terms: Vec<String> = result
        .dice
        .iter()
        .map(|d| {
            let kept = join(&d.results);
            let shown = if d.dropped.is_empty() {
                kept
            } else {
                format!("{kept} / {}", join(&d.dropped))
            };
            format!("{}[{shown}]", if d.sign < 0 { "- " } else { "" })
        })
        .collect();
    match result.modifier {
        0 => {}
        m if m > 0 => terms.push(format!("+ {m}")),
        m => terms.push(format!("- {}", -m)),
    }
    let detail = terms.join(" ");
    detail
        .strip_prefix("+ ")
        .map(str::to_string)
        .unwrap_or(detail)
}

fn join(numbers: &[u32]) -> String {
    numbers
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hands out the given faces in order.
    fn faces(list: &[u32]) -> impl FnMut(u32) -> u32 + '_ {
        let mut next = list.iter();
        move |_| *next.next().expect("enough faces for the test")
    }

    #[test]
    fn reads_dice_and_modifiers() {
        let dice = parse_dice("1d8 + 1d6 - 1").unwrap();
        assert_eq!(dice.terms.len(), 2);
        assert_eq!(dice.modifier, -1);
        assert_eq!(parse_dice("d20").unwrap().terms[0].count, 1);
        assert_eq!(parse_dice("4d6kh3").unwrap().terms[0].keep, Some(3));
        assert_eq!(parse_dice("5").unwrap().modifier, 5);
        assert_eq!(parse_dice("-2d4").unwrap().terms[0].sign, -1);
    }

    #[test]
    fn refuses_what_isnt_a_roll() {
        for text in [
            "", "abc", "2d", "d", "1d1", "101d6", "1d1001", "4d6kh5", "4d6kh0", "1d20++2", "1d20+",
            "2x3",
        ] {
            assert_eq!(parse_dice(text), None, "{text}");
        }
    }

    #[test]
    fn adds_up_a_roll() {
        let result = roll(
            &parse_dice("2d6+3").unwrap(),
            Advantage::Normal,
            false,
            faces(&[4, 5]),
        );
        assert_eq!(result.total, 12);
        assert_eq!(result.natural, None);
        assert_eq!(roll_detail(&result), "[4, 5] + 3");
    }

    #[test]
    fn keeps_the_better_d20_with_advantage_and_the_worse_with_disadvantage() {
        let d20 = parse_dice("1d20+5").unwrap();
        let up = roll(&d20, Advantage::Advantage, false, faces(&[7, 15]));
        assert_eq!((up.total, up.natural), (20, Some(15)));
        assert_eq!(roll_detail(&up), "[15 / 7] + 5");
        let down = roll(&d20, Advantage::Disadvantage, false, faces(&[7, 15]));
        assert_eq!(down.total, 12);
        assert_eq!(down.dice[0].dropped, vec![15]);
    }

    #[test]
    fn doubles_the_dice_on_a_critical() {
        let result = roll(
            &parse_dice("1d8+2").unwrap(),
            Advantage::Normal,
            true,
            faces(&[3, 6]),
        );
        assert_eq!(result.total, 11);
        assert_eq!(result.dice[0].results, vec![3, 6]);
    }

    #[test]
    fn drops_the_lowest_for_keep_highest() {
        let result = roll(
            &parse_dice("4d6kh3").unwrap(),
            Advantage::Normal,
            false,
            faces(&[2, 6, 2, 5]),
        );
        assert_eq!(result.total, 13);
        assert_eq!(result.dice[0].results, vec![6, 2, 5]);
        assert_eq!(result.dice[0].dropped, vec![2]);
    }

    #[test]
    fn writes_a_negative_term_and_modifier() {
        let result = roll(
            &parse_dice("1d6-1d4-2").unwrap(),
            Advantage::Normal,
            false,
            faces(&[5, 3]),
        );
        assert_eq!(result.total, 0);
        assert_eq!(roll_detail(&result), "[5] - [3] - 2");
    }
}
