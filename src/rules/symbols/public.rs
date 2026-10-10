//! Library public API: symbols an outside caller can reach by declaration (#489).

use std::collections::{HashMap, HashSet};

use crate::parser::{ImportKind, ParsedModule};

use super::exports::ReExport;
use super::graph::{ReferenceIndex, RegistryEntry};

/// Modules whose names a public module re-exports with `from m import *`.
#[derive(Debug, Default)]
pub(super) struct PublicApi {
    star_reached: HashSet<String>,
    declares_all: HashSet<String>,
    /// References that `--production` left out of reachability (#588).
    production_references: ReferenceIndex,
}

impl PublicApi {
    pub(super) fn build(
        modules: &[&ParsedModule],
        module_names: &HashMap<&str, String>,
        production_references: ReferenceIndex,
    ) -> Self {
        let mut star_targets: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut declares_all = HashSet::new();
        for module in modules {
            let Some(name) = module_names.get(module.path.as_str()) else {
                continue;
            };
            if !module.exports.is_empty() {
                declares_all.insert(name.clone());
            }
            star_targets
                .entry(name.as_str())
                .or_default()
                .extend(star_imports(module).filter(|target| target != name));
        }

        let seeds = star_targets
            .iter()
            .filter(|(importer, _)| is_public_module(importer))
            .flat_map(|(_, targets)| targets.iter().copied())
            .collect();

        Self {
            star_reached: star_closure(&star_targets, seeds),
            declares_all,
            production_references,
        }
    }

    /// `__all__`, a public package's `__init__`, or `import *` into a public
    /// module puts the symbol in the library's API, as does an outside caller
    /// importing it from a public module.
    pub(super) fn exports_symbol(&self, entry: &RegistryEntry) -> bool {
        let module = entry.id.module.as_str();
        entry.in_all
            || (entry.path.ends_with("__init__.py") && is_public_module(module))
            || (self.star_reached.contains(module) && !self.declares_all.contains(module))
            || (is_public_module(module)
                && self
                    .production_references
                    .is_externally_referenced(&entry.id))
    }
}

pub(super) fn star_imports(module: &ParsedModule) -> impl Iterator<Item = &str> {
    module
        .imports
        .iter()
        .filter(|import| {
            import.kind == ImportKind::ImportFrom
                && import.name.as_deref() == Some("*")
                && !import.module.is_empty()
        })
        .map(|import| import.module.as_str())
}

/// `seeds` and every module they reach through chained star imports.
pub(super) fn star_closure<'a>(
    star_targets: &HashMap<&str, Vec<&'a str>>,
    mut seeds: Vec<&'a str>,
) -> HashSet<String> {
    let mut reached = HashSet::new();
    while let Some(target) = seeds.pop() {
        if reached.insert(target.to_owned()) {
            seeds.extend(star_targets.get(target).into_iter().flatten().copied());
        }
    }
    reached
}

/// Every re-export sits in an `__init__`, so a public package's one is API.
pub(super) fn exports_reexport(reexport: &ReExport) -> bool {
    reexport.declared_public || is_public_module(&reexport.package_module)
}

pub(super) fn is_public_module(module: &str) -> bool {
    module.split('.').all(|part| !part.starts_with('_'))
}
