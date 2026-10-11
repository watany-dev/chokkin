//! Symbol identity and registry for usage analysis.

use std::collections::{HashMap, HashSet};

use crate::parser::{ImportKind, ParsedModule, SymbolDef};

use super::public::star_imports;

/// Rules-local symbol identifier (distinct from graph `SymbolId` if added later).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct SymbolId {
    /// Dotted module name.
    pub module: String,
    /// Symbol name within the module.
    pub name: String,
}

impl SymbolId {
    /// Creates a symbol id from module and name parts.
    #[must_use]
    pub(super) fn new(module: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            name: name.into(),
        }
    }
}

/// One registered public symbol in a reachable module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegistryEntry {
    pub id: SymbolId,
    pub path: String,
    pub def: SymbolDef,
    pub in_all: bool,
}

/// Build the list of public symbols in reachable modules, first definition wins.
pub(super) fn build_registry(
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
) -> Vec<RegistryEntry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    for module in modules {
        let Some(owner) = module_names.get(module.path.as_str()) else {
            continue;
        };
        for symbol in &module.symbols {
            if !symbol.is_public || symbol.in_type_checking {
                continue;
            }
            let id = SymbolId::new(owner.clone(), symbol.name.clone());
            if !seen.insert(id.clone()) {
                continue;
            }
            let in_all = module.exports.iter().any(|export| export == &symbol.name);
            entries.push(RegistryEntry {
                id,
                path: module.path.clone(),
                def: symbol.clone(),
                in_all,
            });
        }
    }

    entries
}

/// Precomputed lookup of symbol references collected from import statements.
///
/// Usage checks run once per registered symbol, so scanning a reference list
/// each time is quadratic in project size; this index is built in one pass
/// over the parsed modules and resolves each lookup in O(1). The value marks
/// whether the symbol was referenced from a module other than its own.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct ReferenceIndex {
    entries: HashMap<SymbolId, bool>,
}

impl ReferenceIndex {
    /// Collect symbol references from `from … import name` statements and
    /// attribute access on module bindings (`import module; module.name`,
    /// `from package import module; module.name`).
    ///
    /// Modules without a module name (e.g. `tests/` without `__init__.py`)
    /// still count as importers: their path never equals a module name, so
    /// every reference they make is external.
    pub(super) fn build(modules: &[&ParsedModule], module_names: &HashMap<&str, String>) -> Self {
        let mut index = Self::default();
        let known_modules: HashSet<&str> = module_names.values().map(String::as_str).collect();
        let mut stars = StarImports::build(modules, module_names);

        for module in modules {
            let importer = module_names
                .get(module.path.as_str())
                .map_or(module.path.as_str(), String::as_str);
            for import in &module.imports {
                if import.module.is_empty() {
                    continue;
                }
                match import.kind {
                    ImportKind::ImportFrom => {
                        let (package, name) = match &import.name {
                            Some(name) => (import.module.as_str(), name.as_str()),
                            // `from . import x` carries no `name`: the parser folds `x` into `module`.
                            None => match import.module.rsplit_once('.') {
                                Some(parts) => parts,
                                None => continue,
                            },
                        };
                        // `from . import x` in the package's own `__init__` defines a re-export, not a use.
                        if import.name.is_some() || package != importer {
                            index.record(importer, SymbolId::new(package, name), &mut stars);
                        }
                        let submodule = format!("{package}.{name}");
                        if known_modules.contains(submodule.as_str()) {
                            let binding = import.alias.as_deref().unwrap_or(name);
                            for target in accesses(module, &submodule, &[binding]) {
                                index.record(importer, target, &mut stars);
                            }
                        }
                    },
                    ImportKind::Import => {
                        let binding = import.alias.as_deref().unwrap_or(&import.module);
                        let receivers = [import.module.as_str(), binding];
                        for target in accesses(module, &import.module, &receivers) {
                            index.record(importer, target, &mut stars);
                        }
                    },
                }
            }
        }

        index
    }

    /// `pkg.name` also uses `name` in each module `pkg` star-imports it from
    /// (#576); the importer, not `pkg`, decides whether that use is external.
    fn record(&mut self, importer: &str, target: SymbolId, stars: &mut StarImports<'_>) {
        for &source in stars.sources(&target) {
            self.mark(importer, SymbolId::new(source, target.name.clone()));
        }
        self.mark(importer, target);
    }

    fn mark(&mut self, importer: &str, target: SymbolId) {
        let external = importer != target.module;
        self.entries
            .entry(target)
            .and_modify(|is_external| *is_external |= external)
            .or_insert(external);
    }

    /// Returns `true` when `target` is referenced from any module.
    pub(super) fn is_referenced(&self, target: &SymbolId) -> bool {
        self.entries.contains_key(target)
    }

    /// Returns `true` when `target` is referenced from a different module.
    pub(super) fn is_externally_referenced(&self, target: &SymbolId) -> bool {
        matches!(self.entries.get(target), Some(true))
    }
}

/// `target_module.name` read through any of `receivers` in `module`.
fn accesses<'m>(
    module: &'m ParsedModule,
    target_module: &'m str,
    receivers: &'m [&'m str],
) -> impl Iterator<Item = SymbolId> + 'm {
    module
        .attribute_accesses
        .iter()
        .filter(|access| receivers.contains(&access.receiver.as_str()))
        .map(move |access| SymbolId::new(target_module, access.name.clone()))
}

/// `from m import *` edges between named modules.
#[derive(Default)]
struct StarImports<'a> {
    importers: HashMap<&'a str, StarImporter<'a>>,
    /// `__all__` of the modules that declare one.
    exports: HashMap<&'a str, HashSet<&'a str>>,
    cache: HashMap<SymbolId, Vec<&'a str>>,
}

#[derive(Default)]
struct StarImporter<'a> {
    targets: Vec<&'a str>,
    /// Names the module defines itself, shadowing star-imported ones.
    defines: HashSet<&'a str>,
}

impl<'a> StarImports<'a> {
    fn build(modules: &[&'a ParsedModule], module_names: &'a HashMap<&str, String>) -> Self {
        let mut stars = Self::default();
        for module in modules {
            let Some(name) = module_names.get(module.path.as_str()) else {
                continue;
            };
            let targets: Vec<_> = star_imports(module)
                .filter(|target| target != name)
                .collect();
            if !targets.is_empty() {
                let importer = stars.importers.entry(name.as_str()).or_default();
                importer.targets.extend(targets);
                importer.defines.extend(
                    module
                        .symbols
                        .iter()
                        .filter(|symbol| !symbol.in_type_checking)
                        .map(|symbol| symbol.name.as_str()),
                );
            }
            if !module.exports.is_empty() {
                stars.exports.insert(
                    name.as_str(),
                    module.exports.iter().map(String::as_str).collect(),
                );
            }
        }
        stars
    }

    /// Cached per symbol: a module that imports a package in many function
    /// bodies records each `pkg.name` access once per import.
    fn sources(&mut self, target: &SymbolId) -> &[&'a str] {
        if !self.importers.contains_key(target.module.as_str()) {
            return &[];
        }
        if !self.cache.contains_key(target) {
            let sources = self.walk(&target.module, &target.name);
            self.cache.insert(target.clone(), sources);
        }
        &self.cache[target]
    }

    /// Modules `module` reaches `name` in through chained star imports. Each
    /// hop hands on only the names in its `__all__`, or without one, the
    /// names not starting with `_`.
    fn walk(&self, module: &str, name: &str) -> Vec<&'a str> {
        let mut sources = Vec::new();
        let mut queue = vec![module];
        let mut seen = HashSet::new();
        while let Some(current) = queue.pop() {
            let Some(importer) = self.importers.get(current) else {
                continue;
            };
            if importer.defines.contains(name) {
                continue;
            }
            for &target in &importer.targets {
                if seen.insert(target) && self.hands_on(target, name) {
                    sources.push(target);
                    queue.push(target);
                }
            }
        }
        sources
    }

    fn hands_on(&self, module: &str, name: &str) -> bool {
        match self.exports.get(module) {
            Some(all) => all.contains(name),
            None => !name.starts_with('_'),
        }
    }
}
