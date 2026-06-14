//! Symbol identity and registry for usage analysis.

use std::collections::HashMap;

use crate::parser::{ParsedModule, SymbolDef};

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

/// A symbol reference from an import statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SymbolReference {
    /// Module containing the import statement.
    pub importer: String,
    /// Referenced symbol.
    pub target: SymbolId,
    /// 1-based source line.
    pub line: u32,
}

/// Collect symbol references from `from … import name` statements (v0.1 conservative).
pub(super) fn collect_import_references(
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
) -> Vec<SymbolReference> {
    let mut references = Vec::new();

    for module in modules {
        let Some(importer) = module_names.get(module.path.as_str()) else {
            continue;
        };
        for import in &module.imports {
            if import.module.is_empty() {
                continue;
            }
            let Some(name) = import.name.as_ref() else {
                continue;
            };
            references.push(SymbolReference {
                importer: importer.clone(),
                target: SymbolId::new(import.module.clone(), name.clone()),
                line: import.line,
            });
        }
    }

    references
}

/// Returns `true` when `target` is referenced from a different module.
pub(super) fn is_externally_referenced(target: &SymbolId, references: &[SymbolReference]) -> bool {
    references
        .iter()
        .any(|reference| reference.target == *target && reference.importer != target.module)
}
