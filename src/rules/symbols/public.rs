//! Library public API: symbols an outside caller can reach by declaration (#489).

use std::collections::{HashMap, HashSet};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::parser::{Exports, ImportKind, ParsedModule};

use super::exports::ReExport;
use super::graph::{ReferenceIndex, RegistryEntry, SymbolId};

/// What an outside caller can import from the library's public modules.
#[derive(Debug, Default)]
pub(super) struct PublicApi {
    /// Names `from m import *` puts into a public module, by their defining
    /// module.
    star_reached: HashSet<SymbolId>,
    /// References that `--production` left out of reachability (#588).
    production_references: ReferenceIndex,
}

impl PublicApi {
    pub(super) fn build(
        modules: &[&ParsedModule],
        module_names: &HashMap<&str, String>,
        production_references: ReferenceIndex,
    ) -> Self {
        let named: Vec<_> = modules
            .iter()
            .filter_map(|&module| {
                module_names
                    .get(module.path.as_str())
                    .map(|name| (name.as_str(), module))
            })
            .collect();
        let chain = StarChain::build(named.iter().copied());
        let star_reached = named
            .iter()
            .filter(|(name, _)| chain.parents.contains_key(name))
            .flat_map(|&(name, module)| {
                module
                    .symbols
                    .iter()
                    .filter(|symbol| !symbol.in_type_checking)
                    .map(move |symbol| (name, symbol.name.as_str()))
            })
            .filter(|&(name, symbol)| chain.reaches(name, symbol, false, is_public_module))
            .map(|(name, symbol)| SymbolId::new(name, symbol))
            .collect();
        Self {
            star_reached,
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
            || self.star_reached.contains(&entry.id)
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

/// `from m import *` edges between modules, and the `__all__` that decides
/// what each hop hands on.
#[derive(Debug, Default)]
pub(super) struct StarChain<'a> {
    /// Names each star-importing module defines itself, shadowing
    /// star-imported ones.
    pub(super) defines: FxHashMap<&'a str, FxHashSet<&'a str>>,
    /// The modules that star-import each module.
    pub(super) parents: FxHashMap<&'a str, Vec<&'a str>>,
    /// `__all__` of every module, `Absent` included.
    exports: FxHashMap<&'a str, &'a Exports>,
}

impl<'a> StarChain<'a> {
    /// The chain between `modules`, each under the name it is star-imported by.
    pub(super) fn build(modules: impl IntoIterator<Item = (&'a str, &'a ParsedModule)>) -> Self {
        let mut chain = Self::default();
        for (name, module) in modules {
            let targets: Vec<_> = star_imports(module)
                .filter(|&target| target != name)
                .collect();
            for &target in &targets {
                chain.parents.entry(target).or_default().push(name);
            }
            if !targets.is_empty() {
                chain.defines.entry(name).or_default().extend(
                    module
                        .symbols
                        .iter()
                        .filter(|symbol| !symbol.in_type_checking)
                        .map(|symbol| symbol.name.as_str()),
                );
            }
            chain.exports.insert(name, &module.exports);
        }
        chain
    }

    /// Whether `name` in `start` reaches a module `is_goal` accepts through
    /// chained star imports (#576, #960). Each hop hands on the names
    /// `Exports::may_hand_on` allows. With `shadow`, a module defining `name`
    /// itself keeps it from going further up, as reference resolution has it
    /// (§12). That ignores statement order and `try` fallbacks, so the public
    /// API and config names, where a dropped name becomes a finding, go
    /// without it. Walks up from `start`: a name has few binders, while a
    /// package can star-import hundreds of modules.
    pub(super) fn reaches(
        &self,
        start: &str,
        name: &str,
        shadow: bool,
        is_goal: impl Fn(&str) -> bool,
    ) -> bool {
        if !self.hands_on(start, name) {
            return false;
        }
        let mut queue = vec![start];
        let mut seen = FxHashSet::from_iter([start]);
        while let Some(current) = queue.pop() {
            for &parent in self.parents.get(current).into_iter().flatten() {
                if shadow && self.defines[parent].contains(name) {
                    continue;
                }
                if is_goal(parent) {
                    return true;
                }
                if !self.hands_on(parent, name) {
                    continue;
                }
                if seen.insert(parent) {
                    queue.push(parent);
                }
            }
        }
        false
    }

    /// Whether `from module import *` may bind `name`.
    pub(super) fn hands_on(&self, module: &str, name: &str) -> bool {
        self.exports
            .get(module)
            .copied()
            .unwrap_or(&Exports::Absent)
            .may_hand_on(name)
    }
}

/// Every re-export sits in an `__init__`, so a public package's one is API.
pub(super) fn exports_reexport(reexport: &ReExport) -> bool {
    reexport.declared_public || is_public_module(&reexport.package_module)
}

pub(super) fn is_public_module(module: &str) -> bool {
    module.split('.').all(|part| !part.starts_with('_'))
}
