//! Merge entry candidates from all sources with path deduplication.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use crate::sources::FileContext;

use super::types::{EntryCandidate, EntryPlan, EntryRoot};

/// Merge candidates with the same `EntrySpec.path`, combining origins and the
/// union of their symbols.
/// The result is sorted by path.
#[must_use]
pub(super) fn merge_entry_candidates(candidates: Vec<EntryCandidate>) -> Vec<EntryRoot> {
    let mut merged: BTreeMap<String, EntryRoot> = BTreeMap::new();

    for candidate in candidates {
        match merged.entry(candidate.spec.path.clone()) {
            Entry::Occupied(mut slot) => merge_candidate(slot.get_mut(), candidate),
            Entry::Vacant(slot) => {
                slot.insert(EntryRoot::from(candidate));
            },
        }
    }

    merged.into_values().collect()
}

impl EntryPlan {
    /// Add `candidate` to the roots, merging it into the root for the same
    /// path as [`merge_entry_candidates`] does. Roots are left unsorted; the
    /// caller sorts once after its last push.
    pub(super) fn push_candidate(&mut self, candidate: EntryCandidate) {
        match self
            .roots
            .iter_mut()
            .find(|root| root.path == candidate.spec.path)
        {
            Some(root) => merge_candidate(root, candidate),
            None => self.roots.push(EntryRoot::from(candidate)),
        }
    }
}

impl From<EntryCandidate> for EntryRoot {
    fn from(candidate: EntryCandidate) -> Self {
        Self {
            path: candidate.spec.path,
            symbols: candidate.spec.symbol.into_iter().collect(),
            context: candidate.context,
            origins: vec![candidate.origin],
        }
    }
}

/// Fold `candidate` into the root for its path: every symbol and origin is
/// kept, and a runtime candidate makes the root runtime.
fn merge_candidate(root: &mut EntryRoot, candidate: EntryCandidate) {
    root.symbols.extend(candidate.spec.symbol);
    if !root.origins.contains(&candidate.origin) {
        root.origins.push(candidate.origin);
    }
    root.context = prefer_context(root.context, candidate.context);
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
                    site: None,
                },
            },
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].origins.len(), 2);
    }

    #[test]
    fn merges_every_symbol_named_for_one_path() {
        let script = |name: &str, symbol: &str| EntryCandidate {
            spec: EntrySpec {
                path: "tool/cli.py".to_owned(),
                symbol: Some(symbol.to_owned()),
            },
            context: FileContext::Runtime,
            origin: EntryOrigin::Manifest {
                name: name.to_owned(),
                group: "console".to_owned(),
            },
        };
        let merged = merge_entry_candidates(vec![
            script("a", "main_a"),
            script("b", "main_b"),
            script("c", "main_a"),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0]
                .symbols
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["main_a", "main_b"]
        );
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

    #[test]
    fn pushed_candidate_merges_like_a_collected_one() {
        let candidate = |context, origin| EntryCandidate {
            spec: EntrySpec {
                path: "tool/cli.py".to_owned(),
                symbol: Some("main".to_owned()),
            },
            context,
            origin,
        };
        let mut plan = EntryPlan {
            mode: crate::config::ProjectMode::App,
            roots: merge_entry_candidates(vec![candidate(
                FileContext::Dev,
                EntryOrigin::Manifest {
                    name: "path".to_owned(),
                    group: "hatch".to_owned(),
                },
            )]),
            warnings: Vec::new(),
            library_members: Vec::new(),
            distributed_members: Vec::new(),
        };
        plan.push_candidate(candidate(FileContext::Runtime, EntryOrigin::Script));
        plan.push_candidate(candidate(FileContext::Dev, EntryOrigin::Script));
        assert_eq!(plan.roots.len(), 1);
        assert_eq!(plan.roots[0].context, FileContext::Runtime);
        assert_eq!(plan.roots[0].origins.len(), 2);
    }
}
