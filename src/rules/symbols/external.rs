//! Mark symbols as externally used (decorators, entry points, plugins).

use std::collections::HashMap;

use indexmap::IndexSet;

use crate::entry::EntryPlan;
use crate::parser::file_module_name;
use crate::plugins::PluginHints;
use crate::sources::LayoutInfo;

use super::graph::{SymbolId, SymbolRegistry};

/// Collect symbols that must be treated as used without import references.
pub(super) fn collect_external_symbols(
    registry: &SymbolRegistry,
    entry: &EntryPlan,
    plugins: &PluginHints,
    module_names: &HashMap<&str, String>,
    layout: &LayoutInfo,
) -> IndexSet<SymbolId> {
    let mut external = IndexSet::new();

    for entry in &entry.roots {
        let Some(symbol) = entry.spec.symbol.as_ref() else {
            continue;
        };
        if let Some(module) = module_names
            .get(entry.spec.path.as_str())
            .cloned()
            .or_else(|| file_module_name(&entry.spec.path, layout))
        {
            external.insert(SymbolId::new(module, symbol.clone()));
        }
    }

    for reference in plugins.symbol_refs() {
        external.insert(SymbolId::new(
            reference.module.clone(),
            reference.symbol.clone(),
        ));
    }

    for entry in registry.entries() {
        if entry.def.decorators.is_empty() {
            continue;
        }
        external.insert(entry.id.clone());
    }

    external
}
