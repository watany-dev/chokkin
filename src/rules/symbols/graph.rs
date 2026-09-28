//! Symbol identity and registry for usage analysis.

use std::collections::{HashMap, HashSet};

use crate::parser::{ImportKind, ParsedModule, SymbolDef};

/// Rules-local symbol identifier (distinct from graph `SymbolId` if added later).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolId {
    /// Dotted module name.
    pub module: String,
    /// Symbol name within the module.
    pub name: String,
}

impl SymbolId {
    /// Creates a symbol id from module and name parts.
    #[must_use]
    pub fn new(module: impl Into<String>, name: impl Into<String>) -> Self {
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

/// Registry of public symbols in reachable modules.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct SymbolRegistry {
    entries: Vec<RegistryEntry>,
    by_id: HashMap<SymbolId, usize>,
}

impl SymbolRegistry {
    /// Returns all registered symbols.
    pub(super) fn entries(&self) -> &[RegistryEntry] {
        &self.entries
    }
}

/// Build a symbol registry from reachable parsed modules.
pub(super) fn build_registry(
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
) -> SymbolRegistry {
    let mut registry = SymbolRegistry::default();

    for module in modules {
        let Some(owner) = module_names.get(module.path.as_str()) else {
            continue;
        };
        for symbol in &module.symbols {
            if !symbol.is_public || symbol.in_type_checking {
                continue;
            }
            let id = SymbolId::new(owner.clone(), symbol.name.clone());
            if registry.by_id.contains_key(&id) {
                continue;
            }
            let in_all = module.exports.iter().any(|export| export == &symbol.name);
            let index = registry.entries.len();
            registry.entries.push(RegistryEntry {
                id: id.clone(),
                path: module.path.clone(),
                def: symbol.clone(),
                in_all,
            });
            registry.by_id.insert(id, index);
        }
    }

    registry
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
                            index.record(importer, SymbolId::new(package, name));
                        }
                        let submodule = format!("{package}.{name}");
                        if known_modules.contains(submodule.as_str()) {
                            let binding = import.alias.as_deref().unwrap_or(name);
                            index.record_accesses(importer, module, &submodule, &[binding]);
                        }
                    },
                    ImportKind::Import => {
                        let binding = import.alias.as_deref().unwrap_or(&import.module);
                        index.record_accesses(
                            importer,
                            module,
                            &import.module,
                            &[import.module.as_str(), binding],
                        );
                    },
                }
            }
        }

        index
    }

    fn record_accesses(
        &mut self,
        importer: &str,
        module: &ParsedModule,
        target_module: &str,
        receivers: &[&str],
    ) {
        for access in &module.attribute_accesses {
            if receivers.contains(&access.receiver.as_str()) {
                self.record(importer, SymbolId::new(target_module, access.name.clone()));
            }
        }
    }

    fn record(&mut self, importer: &str, target: SymbolId) {
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
