//! What a player changed on their own sheet, for the DM's log. The website lists a sheet's
//! numbers and gear as facts; the host compares the facts it had with the ones it gets, so it
//! needs no rules of its own. (The same comparison as the website's, for its own tables.)

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub key: String,
    pub label: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub key: String,
    pub label: String,
    /// None when it wasn't there before (or isn't now).
    pub from: Option<String>,
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// What differs between two lists of facts, in the order the newer one lists them.
pub fn changes(before: &[Fact], after: &[Fact]) -> Vec<Change> {
    let mut found = Vec::new();
    for fact in after {
        let was = before.iter().find(|f| f.key == fact.key);
        if was.is_some_and(|was| was.value == fact.value) {
            continue;
        }
        found.push(Change {
            key: fact.key.clone(),
            label: fact.label.clone(),
            from: was.map(|w| w.value.clone()),
            to: Some(fact.value.clone()),
            kind: fact.kind.clone(),
        });
    }
    for fact in before {
        if after.iter().any(|f| f.key == fact.key) {
            continue;
        }
        found.push(Change {
            key: fact.key.clone(),
            label: fact.label.clone(),
            from: Some(fact.value.clone()),
            to: None,
            kind: fact.kind.clone(),
        });
    }
    found
}

/// Two rounds of changes as one: each thing keeps where it started and where it ended up, and
/// drops out when it ended where it started.
pub fn merge(earlier: &[Change], later: &[Change]) -> Vec<Change> {
    let mut merged: Vec<Change> = earlier.to_vec();
    for change in later {
        match merged.iter_mut().find(|c| c.key == change.key) {
            Some(first) => {
                let from = first.from.clone();
                *first = Change {
                    from,
                    ..change.clone()
                };
            }
            None => merged.push(change.clone()),
        }
    }
    merged.retain(|c| c.from != c.to);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(key: &str, value: &str) -> Fact {
        Fact {
            key: key.into(),
            label: key.to_uppercase(),
            value: value.into(),
            kind: None,
        }
    }

    #[test]
    fn finds_what_changed_came_and_went() {
        let before = [
            fact("ac", "13"),
            fact("item:rope", "1"),
            fact("hp", "32/32"),
        ];
        let after = [
            fact("ac", "15"),
            fact("hp", "32/32"),
            fact("item:potion", "2"),
        ];
        let found = changes(&before, &after);
        let said: Vec<(&str, Option<&str>, Option<&str>)> = found
            .iter()
            .map(|c| (c.key.as_str(), c.from.as_deref(), c.to.as_deref()))
            .collect();
        assert_eq!(
            said,
            [
                ("ac", Some("13"), Some("15")),
                ("item:potion", None, Some("2")),
                ("item:rope", Some("1"), None)
            ]
        );
    }

    #[test]
    fn folds_saves_together_and_drops_what_came_back() {
        let first = changes(
            &[fact("hp", "32/32"), fact("ac", "13")],
            &[fact("hp", "30/32"), fact("ac", "15")],
        );
        let second = changes(
            &[fact("hp", "30/32"), fact("ac", "15")],
            &[fact("hp", "25/32"), fact("ac", "13")],
        );
        let merged = merge(&first, &second);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            (merged[0].from.as_deref(), merged[0].to.as_deref()),
            (Some("32/32"), Some("25/32"))
        );
    }
}
