//! Mark symbols as externally used (decorators, entry points, plugins).

use std::collections::{HashMap, HashSet};

use indexmap::IndexSet;

use crate::entry::EntryPlan;
use crate::parser::{ParsedModule, SymbolKind};
use crate::plugins::PluginHints;
use crate::sources::{LayoutInfo, path_to_module};

use super::graph::{RegistryEntry, SymbolId};

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
    // click / typer; `@cli.group()` nests a group under `cli`.
    "command",
    "group",
    // `@admin.register`, `@atexit.register`, `@functools.singledispatch`'s
    // `.register`; `register_*` names are matched below.
    "register",
    // flask-restful `@api.representation("application/json")`.
    "representation",
    // SQLAlchemy `@event.listens_for(Engine, "after_execute")`.
    "listens_for",
];

/// Collect symbols that must be treated as used without import references.
pub(super) fn collect_external_symbols(
    registry: &[RegistryEntry],
    entry: &EntryPlan,
    plugins: &PluginHints,
    module_names: &HashMap<&str, String>,
    layout: &LayoutInfo,
) -> IndexSet<SymbolId> {
    let mut external = IndexSet::new();

    for entry in &entry.roots {
        if let Some(module) = module_names
            .get(entry.path.as_str())
            .cloned()
            .or_else(|| path_to_module(&entry.path, layout))
        {
            for symbol in &entry.symbols {
                external.insert(SymbolId::new(module.clone(), target_object(symbol)));
            }
        }
    }

    for reference in plugins.symbol_refs().chain(plugins.used_symbols()) {
        external.insert(SymbolId::new(
            reference.module.clone(),
            target_object(&reference.symbol),
        ));
    }

    for entry in registry {
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

/// The module-level name an `object.attr [extras]` target reads: the loader
/// imports the module, gets `object`, and walks the attributes from there
/// (#872).
fn target_object(symbol: &str) -> &str {
    let symbol = symbol.split_once('[').map_or(symbol, |(name, _)| name);
    symbol
        .split_once('.')
        .map_or(symbol, |(name, _)| name)
        .trim()
}

/// Symbols a string literal names by their full dotted path (#1238): Django's
/// `MIDDLEWARE` and `STORAGES` values, a DRF or drf-spectacular hook, a class
/// list a loader imports with `import_string`, or `pkg.mod:attr`. The literal
/// is a parser [`crate::parser::ModuleRef`]; its longest module-name prefix
/// names the segment after it, so `"pkg.mod.Cls.method"` names `pkg.mod:Cls`.
/// Strings in `skip` (test files, whose `mock.patch("pkg.mod.name")` names a
/// symbol without using it) and a module's strings that name its own symbols
/// count for nothing.
pub(super) fn string_named_symbols(
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
    skip: &HashSet<&str>,
) -> Vec<SymbolId> {
    let known: HashSet<&str> = module_names.values().map(String::as_str).collect();
    let mut named = Vec::new();
    for module in modules {
        if skip.contains(module.path.as_str()) {
            continue;
        }
        let own = module_names.get(module.path.as_str()).map(String::as_str);
        for reference in &module.module_refs {
            let text = reference
                .text
                .strip_prefix("python:")
                .unwrap_or(&reference.text);
            if let Some((owner, name)) = named_symbol(text, &known)
                && Some(owner) != own
            {
                named.push(SymbolId::new(owner.to_owned(), name));
            }
        }
    }
    named
}

/// The `(module, name)` pair `text` names among the `known` modules: the
/// attribute of `module:attr`, else the segment after the longest module-name
/// prefix of a dotted name, where an import of the name would stop.
fn named_symbol<'t>(text: &'t str, known: &HashSet<&str>) -> Option<(&'t str, &'t str)> {
    if let Some((module, attr)) = text.split_once(':') {
        return known
            .contains(module)
            .then(|| (module, target_object(attr)));
    }
    text.rmatch_indices('.')
        .map(|(dot, _)| (&text[..dot], &text[dot + 1..]))
        .find(|(module, _)| known.contains(module))
        .map(|(module, rest)| (module, rest.split_once('.').map_or(rest, |(name, _)| name)))
}

/// Classes a loader may fetch by a computed name: `import_module("pkg." + x)`
/// reaches every module under `pkg`, and a `getattr(module, <expr>)` on the
/// result in the same scope can return any class they define (#728).
pub(super) fn dynamically_fetched_classes<'a>(
    registry: &'a [RegistryEntry],
    modules: &[&ParsedModule],
) -> impl Iterator<Item = SymbolId> + 'a {
    let packages: Vec<String> = modules
        .iter()
        .flat_map(|module| &module.dynamic_import_prefixes)
        .filter(|prefix| prefix.computed_getattr)
        .map(|prefix| format!("{}.", prefix.module))
        .collect();
    registry
        .iter()
        .filter(move |entry| {
            entry.def.kind == SymbolKind::Class
                && packages
                    .iter()
                    .any(|package| entry.id.module.starts_with(package.as_str()))
        })
        .map(|entry| entry.id.clone())
}

/// Whether a normalized decorator name registers its target: any
/// `pytest.mark.*`, a pluggy marker, a `register_*` or `*_handler` name, or a
/// listed suffix with or without a receiver.
fn is_registration_decorator(name: &str) -> bool {
    if name.starts_with("pytest.mark.") {
        return true;
    }
    let suffix = name.rsplit_once('.').map_or(name, |(_, suffix)| suffix);
    // pluggy markers are named for what they mark: `hookimpl`, airflow's
    // `local_settings_hookspec = HookspecMarker(...)` (fixture `pluggy_hooks`).
    REGISTRATION_DECORATOR_SUFFIXES.contains(&suffix)
        || suffix.ends_with("hookimpl")
        || suffix.ends_with("hookspec")
        // A project's own registry decorators (#1238, #1223): weblate's
        // `@register_notification` and `@register_hook`, wger's
        // `@register_handler`, netbox's `@register_search`.
        || suffix.starts_with("register_")
        // flask-login `@login_manager.unauthorized_handler`, FastAPI
        // `@app.exception_handler(...)`.
        || suffix.ends_with("_handler")
}

#[cfg(test)]
mod tests {
    use super::{is_registration_decorator, target_object};

    #[test]
    fn target_object_drops_the_attribute_path_and_extras() {
        for (symbol, object) in [
            ("main", "main"),
            ("Cli.main", "Cli"),
            ("Cli.group.main", "Cli"),
            ("main [cli]", "main"),
            ("Cli.main [cli, extra]", "Cli"),
            ("main[cli]", "main"),
            (" main ", "main"),
        ] {
            assert_eq!(target_object(symbol), object, "{symbol}");
        }
    }

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
            "hookimpl",
            "pm.hookspec",
            "local_settings_hookspec",
            "cli.group",
            "admin.register",
            "api.representation",
            "event.listens_for",
            "listens_for",
            "register_notification",
            "registry.register_search",
            "login_manager.unauthorized_handler",
            "app.exception_handler",
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
            "hookimpl_wrapper",
            "registered",
            "preregister_hook",
            "handler",
            "handlers",
            "groups",
        ] {
            assert!(!is_registration_decorator(name), "{name}");
        }
    }
}
