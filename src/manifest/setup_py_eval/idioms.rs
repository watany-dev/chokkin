//! Project idioms the evaluator recognises outside its whitelist.
//!
//! The evaluator core folds only the subset its module doc lists. An idiom
//! here is a heuristic for a pattern specific to known projects, and runs at a
//! single place: where an expression would otherwise end as
//! [`Value::Unknown`]. Add one only for a project that needs it, with tests
//! in this file.

use std::collections::HashMap;

use super::Value;
use crate::manifest::pep508_util::normalize_distribution_name;

/// What [`requirement_table_lookup`] found for a key.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct TableEntry {
    /// The requirement string the key names.
    pub(super) requirement: String,
    /// Every requirement string in the lists holding it.
    pub(super) table: Vec<String>,
}

/// `deps["tokenizers"]` on a table built at install time (transformers
/// `deps = {b: a for a, b in (re.findall(...)[0] for x in _deps)}`): find the
/// one requirement string in a list of `env` whose name matches the key. The
/// bare key itself (e.g. a `deps_list("torch")` argument) loses to a fuller
/// one.
pub(super) fn requirement_table_lookup(
    env: &HashMap<String, Value>,
    key: &str,
) -> Option<TableEntry> {
    let wanted = normalize_distribution_name(key);
    let mut found: Vec<&String> = Vec::new();
    for items in lists(env) {
        for item in items {
            if let Value::Str(raw) = item
                && normalize_distribution_name(requirement_table_key(raw)) == wanted
                && !found.contains(&raw)
            {
                found.push(raw);
            }
        }
    }
    if found.len() > 1 {
        found.retain(|raw| raw.as_str() != key);
    }
    let [raw] = found.as_slice() else {
        return None;
    };
    let requirement = Value::Str((*raw).clone());
    let table = lists(env)
        .filter(|items| items.contains(&requirement))
        .flatten()
        .filter_map(|item| match item {
            Value::Str(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    Some(TableEntry {
        requirement: (*raw).clone(),
        table,
    })
}

fn lists(env: &HashMap<String, Value>) -> impl Iterator<Item = &Vec<Value>> {
    env.values().filter_map(|value| match value {
        Value::List(items) => Some(items),
        _ => None,
    })
}

/// Requirement text before its version or marker, extras kept.
fn requirement_table_key(raw: &str) -> &str {
    raw.split(['!', '=', '<', '>', '~', ' ', ';', '@'])
        .next()
        .unwrap_or(raw)
        .trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(texts: &[&str]) -> Value {
        Value::List(
            texts
                .iter()
                .map(|text| Value::Str((*text).to_owned()))
                .collect(),
        )
    }

    fn env(vars: &[(&str, Value)]) -> HashMap<String, Value> {
        vars.iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect()
    }

    fn lookup(env: &HashMap<String, Value>, key: &str) -> Option<String> {
        requirement_table_lookup(env, key).map(|entry| entry.requirement)
    }

    #[test]
    fn key_matches_the_normalized_name_before_the_version() {
        let env = env(&[(
            "_deps",
            list(&[
                "Tokenizers>=0.22,<=0.23",
                "torch>=2.2",
                "mistral-common[image]>=1.8",
            ]),
        )]);
        assert_eq!(
            lookup(&env, "tokenizers").as_deref(),
            Some("Tokenizers>=0.22,<=0.23")
        );
        assert_eq!(
            lookup(&env, "mistral-common[image]").as_deref(),
            Some("mistral-common[image]>=1.8")
        );
        assert_eq!(lookup(&env, "numpy"), None);
    }

    #[test]
    fn table_holds_every_string_of_the_lists_with_the_match() {
        let env = env(&[
            (
                "_deps",
                Value::List(vec![
                    Value::Str("torch>=2.2".to_owned()),
                    Value::Unknown,
                    Value::Str("peft>=0.20".to_owned()),
                ]),
            ),
            ("other", list(&["ruff"])),
        ]);
        assert_eq!(
            requirement_table_lookup(&env, "torch"),
            Some(TableEntry {
                requirement: "torch>=2.2".to_owned(),
                table: vec!["torch>=2.2".to_owned(), "peft>=0.20".to_owned()],
            })
        );
    }

    #[test]
    fn bare_key_loses_to_a_pinned_requirement() {
        let env = env(&[("pkgs", list(&["torch"])), ("_deps", list(&["torch>=2.2"]))]);
        assert_eq!(lookup(&env, "torch").as_deref(), Some("torch>=2.2"));
    }

    #[test]
    fn ambiguous_match_is_not_resolved() {
        let env = env(&[("a", list(&["torch>=2.2"])), ("b", list(&["torch<2"]))]);
        assert_eq!(lookup(&env, "torch"), None);
    }

    #[test]
    fn same_string_in_two_lists_is_one_match() {
        let env = env(&[
            ("a", list(&["torch>=2.2"])),
            ("b", list(&["torch>=2.2", "numpy"])),
        ]);
        let entry = requirement_table_lookup(&env, "torch").expect("found");
        assert_eq!(entry.requirement, "torch>=2.2");
        assert_eq!(entry.table.len(), 3);
    }

    #[test]
    fn only_strings_directly_in_lists_are_searched() {
        let env = env(&[
            ("name", Value::Str("torch>=2.2".to_owned())),
            (
                "extras",
                Value::Dict(vec![("dev".to_owned(), list(&["torch>=2.2"]))]),
            ),
            ("nested", Value::List(vec![list(&["torch>=2.2"])])),
        ]);
        assert_eq!(lookup(&env, "torch"), None);
    }
}
