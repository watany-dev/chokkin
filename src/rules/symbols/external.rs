//! Mark symbols as externally used (decorators, entry points, plugins).

use std::collections::{HashMap, HashSet};

use indexmap::IndexSet;

use crate::entry::EntryPlan;
use crate::parser::{ImportRef, ParsedModule, SymbolKind};
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
    // Celery / blinker signals: `@app.on_after_finalize.connect`.
    "connect",
    // flask-login `@login_manager.user_loader`.
    "user_loader",
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

/// Symbols a first-party registry decorator stores (#1223): the decorator
/// resolves, through the decorated module's imports and up to
/// [`MAX_REEXPORT_HOPS`] re-exports, to a function the parser recognized as
/// one ([`ParsedModule::registry_decorators`]). A decorator that resolves to
/// nothing or to another function leaves the symbol to the other checks.
pub(super) fn registered_by(
    registry: &[RegistryEntry],
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
) -> Vec<SymbolId> {
    let by_name: HashMap<&str, &ParsedModule> = modules
        .iter()
        .filter_map(|module| Some((module_names.get(module.path.as_str())?.as_str(), *module)))
        .collect();
    registry
        .iter()
        .filter(|entry| {
            let Some(module) = by_name.get(entry.id.module.as_str()) else {
                return false;
            };
            entry.def.decorators.iter().any(|decorator| {
                decorator_target(&entry.id.module, module, decorator)
                    .is_some_and(|(owner, name)| is_registrar(&by_name, owner, name))
            })
        })
        .map(|entry| entry.id.clone())
        .collect()
}

/// Re-exports followed from where a decorator is imported to where it is
/// defined (`from .base import register`).
const MAX_REEXPORT_HOPS: usize = 3;

/// The `(module, name)` a decorator written in `module` names: a function the
/// module defines, a `from m import name [as alias]` binding, or an attribute
/// of a module binding (`@search.register_search`, `@pkg.search.register`).
fn decorator_target(own: &str, module: &ParsedModule, decorator: &str) -> Option<(String, String)> {
    if decorator.contains('[') {
        return None;
    }
    let (head, rest) = match decorator.split_once('.') {
        Some((head, rest)) => (head, Some(rest)),
        None => (decorator, None),
    };
    let import = module
        .imports
        .iter()
        .rev()
        .find(|import| import.bound_name() == Some(head));
    let Some(rest) = rest else {
        return match import.and_then(ImportRef::import_target) {
            Some((owner, name)) => Some((owner.to_owned(), name.to_owned())),
            None => module
                .symbols
                .iter()
                .any(|symbol| symbol.name == head && symbol.kind == SymbolKind::Function)
                .then(|| (own.to_owned(), head.to_owned())),
        };
    };
    let import = import?;
    let base = match (import.import_target(), &import.alias) {
        (Some((package, name)), _) => format!("{package}.{name}"),
        (None, Some(_)) => import.module.clone(),
        (None, None) => head.to_owned(),
    };
    let path = format!("{base}.{rest}");
    let (owner, name) = path.rsplit_once('.')?;
    Some((owner.to_owned(), name.to_owned()))
}

/// Whether `owner.name` is a registry decorator, following `from m import
/// name` re-exports in `owner`.
fn is_registrar(by_name: &HashMap<&str, &ParsedModule>, owner: String, name: String) -> bool {
    let (mut owner, mut name) = (owner, name);
    for _ in 0..=MAX_REEXPORT_HOPS {
        let Some(module) = by_name.get(owner.as_str()) else {
            return false;
        };
        if module.registry_decorators.contains(&name) {
            return true;
        }
        let Some((next_owner, next_name)) = module
            .imports
            .iter()
            .rev()
            .filter(|import| import.bound_name() == Some(name.as_str()))
            .find_map(ImportRef::import_target)
        else {
            return false;
        };
        (owner, name) = (next_owner.to_owned(), next_name.to_owned());
    }
    false
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
    use std::collections::HashMap;

    use super::{decorator_target, is_registrar, is_registration_decorator, target_object};
    use crate::parser::{
        ImportContext, ImportKind, ImportRef, ParsedModule, SymbolDef, SymbolKind,
    };

    fn import(
        kind: ImportKind,
        module: &str,
        name: Option<&str>,
        alias: Option<&str>,
    ) -> ImportRef {
        ImportRef {
            module: module.to_owned(),
            name: name.map(str::to_owned),
            alias: alias.map(str::to_owned),
            line: 1,
            kind,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            deferred: false,
            relative_level: 0,
            fallback_for: Vec::new(),
            version_minors: None,
        }
    }

    fn function(name: &str) -> SymbolDef {
        SymbolDef {
            name: name.to_owned(),
            kind: SymbolKind::Function,
            line: 1,
            is_public: true,
            decorators: Vec::new(),
            in_type_checking: false,
            used_in_module: false,
        }
    }

    #[test]
    fn decorator_targets_follow_the_module_bindings() {
        let module = ParsedModule {
            imports: vec![
                import(
                    ImportKind::ImportFrom,
                    "app.reg",
                    Some("register"),
                    Some("reg"),
                ),
                import(ImportKind::ImportFrom, "app", Some("search"), None),
                import(ImportKind::Import, "app.hooks", None, None),
                import(ImportKind::Import, "app.perms", None, Some("perms")),
            ],
            symbols: vec![function("local"), function("Other")],
            ..ParsedModule::default()
        };
        for (decorator, target) in [
            ("reg", Some(("app.reg", "register"))),
            (
                "search.register_search",
                Some(("app.search", "register_search")),
            ),
            ("app.hooks.hook", Some(("app.hooks", "hook"))),
            ("perms.perm", Some(("app.perms", "perm"))),
            ("local", Some(("app.views", "local"))),
            ("unknown", None),
            ("unknown.attr", None),
            ("apps[].route", None),
        ] {
            let found = decorator_target("app.views", &module, decorator);
            assert_eq!(
                found.as_ref().map(|(m, n)| (m.as_str(), n.as_str())),
                target,
                "{decorator}"
            );
        }
    }

    #[test]
    fn registrars_are_found_through_reexports() {
        let defining = ParsedModule {
            registry_decorators: vec!["register".to_owned()],
            ..ParsedModule::default()
        };
        let reexporting = ParsedModule {
            imports: vec![import(
                ImportKind::ImportFrom,
                "app.base",
                Some("register"),
                None,
            )],
            ..ParsedModule::default()
        };
        let by_name: HashMap<&str, &ParsedModule> =
            [("app.base", &defining), ("app", &reexporting)]
                .into_iter()
                .collect();
        assert!(is_registrar(
            &by_name,
            "app.base".to_owned(),
            "register".to_owned()
        ));
        assert!(is_registrar(
            &by_name,
            "app".to_owned(),
            "register".to_owned()
        ));
        assert!(!is_registrar(
            &by_name,
            "app".to_owned(),
            "other".to_owned()
        ));
        assert!(!is_registrar(
            &by_name,
            "missing".to_owned(),
            "register".to_owned()
        ));
    }

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
            "app.on_after_finalize.connect",
            "login_manager.user_loader",
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
