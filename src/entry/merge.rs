//! Merge entry candidates from all sources with path deduplication.

use std::collections::BTreeMap;

use crate::config::EntrySpec;
use crate::sources::FileContext;

use super::types::{EntryCandidate, EntryRoot};

/// Merge candidates with the same `EntrySpec.path`, combining origins and symbols.
#[must_use]
pub fn merge_entry_candidates(candidates: Vec<EntryCandidate>) -> Vec<EntryRoot> {
    let mut merged: BTreeMap<String, EntryRoot> = BTreeMap::new();

    for candidate in candidates {
        let path = candidate.spec.path.clone();
        match merged.get_mut(&path) {
            Some(root) => {
                merge_symbol(&mut root.spec, &candidate.spec);
                if !root.origins.contains(&candidate.origin) {
                    root.origins.push(candidate.origin);
                }
                root.context = prefer_context(root.context, candidate.context);
            },
            None => {
                merged.insert(
                    path,
                    EntryRoot {
                        spec: candidate.spec,
                        context: candidate.context,
                        origins: vec![candidate.origin],
                    },
                );
            },
        }
    }

    merged.into_values().collect()
}

fn merge_symbol(target: &mut EntrySpec, incoming: &EntrySpec) {
    match (&target.symbol, &incoming.symbol) {
        (None, Some(symbol)) => target.symbol = Some(symbol.clone()),
        (Some(existing), Some(new_symbol)) if existing != new_symbol => {
            target.symbol = Some(new_symbol.clone());
        },
        _ => {},
    }
}

fn prefer_context(existing: FileContext, incoming: FileContext) -> FileContext {
    if existing == FileContext::Runtime || incoming == FileContext::Runtime {
        FileContext::Runtime
    } else {
        existing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EntrySpec, PluginId};

    use super::super::types::EntryOrigin;

    #[test]
    fn merges_same_path_origins() {
        let merged = merge_entry_candidates(vec![
            EntryCandidate {
                spec: EntrySpec {
                    path: "manage.py".to_owned(),
                    symbol: None,
                },
                context: FileContext::Runtime,
                origin: EntryOrigin::Auto {
                    rule: "manage.py".to_owned(),
                },
            },
            EntryCandidate {
                spec: EntrySpec {
                    path: "manage.py".to_owned(),
                    symbol: None,
                },
                context: FileContext::Runtime,
                origin: EntryOrigin::Plugin {
                    plugin: PluginId::Django,
                    label: "manage.py".to_owned(),
                },
            },
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].origins.len(), 2);
    }

    #[test]
    fn keeps_distinct_paths() {
        let merged = merge_entry_candidates(vec![
            EntryCandidate {
                spec: EntrySpec {
                    path: "manage.py".to_owned(),
                    symbol: None,
                },
                context: FileContext::Runtime,
                origin: EntryOrigin::Auto {
                    rule: "manage.py".to_owned(),
                },
            },
            EntryCandidate {
                spec: EntrySpec {
                    path: "src/acme/asgi.py".to_owned(),
                    symbol: None,
                },
                context: FileContext::Runtime,
                origin: EntryOrigin::Auto {
                    rule: "asgi.py".to_owned(),
                },
            },
        ]);
        assert_eq!(merged.len(), 2);
    }
}
