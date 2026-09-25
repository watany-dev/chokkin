//! Mark symbols as externally used (decorators, entry points, plugins).

use std::collections::HashMap;

use indexmap::IndexSet;

use crate::entry::EntryPlan;
use crate::plugins::PluginHints;
use crate::sources::{LayoutInfo, path_to_module};

use super::graph::{SymbolId, SymbolRegistry};

/// Decorator suffixes that register the decorated symbol with a framework,
/// which then calls it without an import reference (spec §14 "decorators").
///
/// The parser records every decorator and the Flask / Celery plugins apply
/// their own stricter predicates for module references, so this is the one
/// list deciding external use. Extend it only with a regression fixture under
/// `tests/fixtures/symbols/` that shows the false positive.
const REGISTRATION_DECORATOR_SUFFIXES: &[&str] = &[
    // FastAPI / Flask routes (`@app.get`, `@router.post`, `@bp.route`).
    "get",
    "post",
    "put",
    "delete",
    "patch",
    "route",
    // FastAPI `@router.websocket("/ws")` (fixture `fastapi_websocket`).
    "websocket",
    // pytest.
    "fixture",
    // Celery.
    "shared_task",
    "task",
    // click / typer.
    "command",
];

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
            .or_else(|| path_to_module(&entry.spec.path, layout))
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
        if entry
            .def
            .decorators
            .iter()
            .any(|name| is_registration_decorator(name.as_str()))
        {
            external.insert(entry.id.clone());
        }
    }

    external
}

/// Whether a normalized decorator name registers its target: any
/// `pytest.mark.*`, or a listed suffix with or without a receiver.
fn is_registration_decorator(name: &str) -> bool {
    if name.starts_with("pytest.mark.") {
        return true;
    }
    let suffix = name.rsplit_once('.').map_or(name, |(_, suffix)| suffix);
    REGISTRATION_DECORATOR_SUFFIXES.contains(&suffix)
}

#[cfg(test)]
mod tests {
    use super::is_registration_decorator;

    #[test]
    fn registration_decorators_are_recognized() {
        for name in [
            "app.get",
            "apps[].route",
            "router.websocket",
            "pytest.fixture",
            "pytest.mark.parametrize",
            "shared_task",
            "celery.task",
            "click.command",
        ] {
            assert!(is_registration_decorator(name), "{name}");
        }
    }

    #[test]
    fn plain_decorators_are_not_registrations() {
        for name in [
            "functools.lru_cache",
            "property",
            "staticmethod",
            "shared_task_wrapper",
            "app.tasks",
        ] {
            assert!(!is_registration_decorator(name), "{name}");
        }
    }
}
