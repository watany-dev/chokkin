//! Modules Django loads by convention from each installed app (#399).

use std::collections::BTreeMap;
use std::path::Path;

use ruff_python_ast::{Expr, Stmt};

use crate::manifest::literals::{assigned_value, parse_module, string_value};
use crate::manifest::util::read_to_string;
use crate::parser::ParsedModule;

use super::super::types::{ModuleReference, PluginContribution, ReferenceOrigin};
use super::super::util::{decorator_suffix, push_used_symbol};
use super::{Lookup, PluginContext, abs_path, module_path};

/// The `INSTALLED_APPS` strings read from the settings, each with where it
/// is listed.
#[derive(Default)]
pub(super) struct Installed {
    pub apps: Vec<(String, ReferenceOrigin)>,
    /// `true` when the settings may install an app `apps` does not show.
    pub incomplete: bool,
}

/// What `@receiver` decorates is connected to a signal once its module is
/// imported, in an installed app or not; the decorator is the reference.
pub(super) fn receivers(ctx: &PluginContext<'_>, contrib: &mut PluginContribution) {
    for module in &ctx.parse.modules {
        let Some(name) = crate::sources::path_to_module(&module.path, &ctx.sources.layout) else {
            continue;
        };
        for symbol in &module.symbols {
            if symbol
                .decorators
                .iter()
                .any(|decorator| decorator_suffix(decorator).1 == "receiver")
            {
                let origin = ReferenceOrigin {
                    file: module.path.clone(),
                    line: Some(symbol.line),
                    label: "Django signal receiver".to_owned(),
                };
                push_used_symbol(contrib, &name, &symbol.name, &origin);
            }
        }
    }
}

/// Make the modules Django imports from each first-party installed app
/// reachable, and the names it reads from them used:
///
/// - `apps.py` and the `AppConfig` subclasses in it (app registry),
/// - `models.py` / `models/__init__.py` (app registry),
/// - `admin.py` / `admin/__init__.py`, when the admin is installed (admin
///   autodiscovery),
/// - each module under `templatetags/` that binds `register`, and the
///   `register` library and the functions it decorates (template engine),
/// - each module directly in `management/commands/` that binds `Command`
///   and does not start with `_`, and its `Command` (`manage.py`).
///
/// In any module of the app, what `@register` decorates is used too.
///
/// A templatetag or command module that does not bind the name Django reads
/// fails to load there, so it is left to the import graph; one that does
/// not parse may bind it and is reachable. An app that is not a project
/// package (third party, or a value that names neither a package nor an
/// `AppConfig` in a project module) adds nothing, as before. Signals and
/// checks are reached through the imports in `AppConfig.ready()`.
pub(super) fn extract(
    ctx: &PluginContext<'_>,
    lookup: &Lookup,
    installed: &Installed,
    contrib: &mut PluginContribution,
) {
    // An `INSTALLED_APPS` that was not read may install the admin, so its
    // modules stay reachable (ADR 0005).
    let mut admin = installed.incomplete
        || installed
            .apps
            .iter()
            .any(|(value, _)| installs_admin(value));
    let mut apps: BTreeMap<String, ReferenceOrigin> = BTreeMap::new();
    for (value, origin) in &installed.apps {
        let Some(app) = resolve_app(ctx, lookup, value) else {
            continue;
        };
        if let Some((module, class)) = &app.config {
            push_used_symbol(contrib, module, class, &relabel(origin, CONFIG_LABEL));
        }
        admin |= app.admin_config;
        if let Some(dir) = app.dir {
            apps.entry(dir).or_insert_with(|| origin.clone());
        }
    }

    for module in &ctx.parse.modules {
        let Some(name) = crate::sources::path_to_module(&module.path, &ctx.sources.layout) else {
            continue;
        };
        // The innermost app, when one is nested in another's package.
        let Some((dir, rest)) = apps
            .keys()
            .filter_map(|dir| {
                let rest = module.path.strip_prefix(dir.as_str())?.strip_prefix('/')?;
                Some((dir, rest))
            })
            .max_by_key(|(dir, _)| dir.len())
        else {
            continue;
        };
        let origin = &apps[dir];
        // Registered with the system checks or the admin; this holds in a
        // module no convention loads, as `ready()` imports `checks.py`.
        let registered = relabel(origin, "Django registration");
        for symbol in decorated(module, |decorator| {
            decorator_suffix(decorator).1 == "register"
        }) {
            push_used_symbol(contrib, &name, &symbol, &registered);
        }
        let (label, used): (&str, Vec<String>) = match rest {
            "apps.py" => (CONFIG_LABEL, config_classes(ctx, &module.path)),
            "models.py" | "models/__init__.py" => ("Django models", Vec::new()),
            "admin.py" | "admin/__init__.py" if admin => ("Django admin", Vec::new()),
            _ if is_templatetag(rest) && binds(module, "register") => {
                let mut used = decorated(module, |decorator| decorator.starts_with("register."));
                used.push("register".to_owned());
                ("Django templatetags", used)
            },
            _ if is_command(rest) && binds(module, "Command") => {
                ("Django management command", vec!["Command".to_owned()])
            },
            _ => continue,
        };
        let origin = relabel(origin, label);
        for symbol in used {
            push_used_symbol(contrib, &name, &symbol, &origin);
        }
        contrib.module_refs.push(ModuleReference {
            module: name,
            origin,
        });
    }
}

const CONFIG_LABEL: &str = "Django app config";

/// An installed value read from the project: the app package directory, the
/// `AppConfig` it names, and whether that config is the admin's.
struct App {
    dir: Option<String>,
    config: Option<(String, String)>,
    admin_config: bool,
}

/// `django.contrib.admin` and its `AdminConfig`, which autodiscovers each
/// app's `admin` module; `SimpleAdminConfig` does not.
fn installs_admin(value: &str) -> bool {
    value == "django.contrib.admin"
        || (value.starts_with("django.contrib.admin.") && !value.ends_with(".SimpleAdminConfig"))
}

/// The project package an `INSTALLED_APPS` value installs: the package
/// itself, or the one an `AppConfig` subclass in a project module names. A
/// literal `name` names a package, which may be no project one; one that is
/// not a literal is taken to name the package of the module that defines the
/// config, where Django apps keep it; with no `name`, the config inherits a
/// third-party one (`AdminConfig`).
fn resolve_app(ctx: &PluginContext<'_>, lookup: &Lookup, value: &str) -> Option<App> {
    let root = ctx.root.path.as_path();
    if let Some(dir) = package_dir(ctx, lookup, value) {
        return Some(App {
            dir: Some(dir),
            config: None,
            admin_config: false,
        });
    }
    let (module, class) = value.rsplit_once('.')?;
    let path = module_path(root, module, ctx, lookup)?;
    let stmts = read_to_string(&abs_path(root, &path))
        .ok()
        .and_then(|contents| parse_module(&contents));
    let class_def = stmts.as_deref()?.iter().find_map(|stmt| match stmt {
        Stmt::ClassDef(def) if def.name.as_str() == class => Some(def),
        _ => None,
    })?;
    let dir = assigned_value(&class_def.body, "name").and_then(|name| match string_value(name) {
        Some(name) => package_dir(ctx, lookup, &name),
        None => path.rsplit_once('/').map(|(dir, _)| dir.to_owned()),
    });
    Some(App {
        dir,
        config: Some((module.to_owned(), class.to_owned())),
        admin_config: class_def
            .bases()
            .iter()
            .any(|base| base_name(base) == Some("AdminConfig")),
    })
}

/// The directory of the project package `module` names. A directory
/// without `__init__.py` is no package to the layout, so its files are not
/// reported either way.
fn package_dir(ctx: &PluginContext<'_>, lookup: &Lookup, module: &str) -> Option<String> {
    module_path(ctx.root.path.as_path(), module, ctx, lookup)?
        .strip_suffix("/__init__.py")
        .map(str::to_owned)
}

/// The last name of a class base (`AppConfig`, `apps.AppConfig`).
fn base_name(base: &Expr) -> Option<&str> {
    match base {
        Expr::Name(name) => Some(name.id.as_str()),
        Expr::Attribute(attribute) => Some(attribute.attr.as_str()),
        _ => None,
    }
}

/// The top-level classes of an app's `apps.py` whose base is an app config
/// (`AppConfig`, `PluginConfig`): Django picks the default one from the
/// module, so no importer names it.
fn config_classes(ctx: &PluginContext<'_>, path: &str) -> Vec<String> {
    read_to_string(&abs_path(ctx.root.path.as_path(), path))
        .ok()
        .and_then(|contents| parse_module(&contents))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|stmt| match stmt {
            Stmt::ClassDef(def)
                if def
                    .bases()
                    .iter()
                    .any(|base| base_name(base).is_some_and(|name| name.ends_with("Config"))) =>
            {
                Some(def.name.to_string())
            },
            _ => None,
        })
        .collect()
}

/// Under `templatetags/`, which Django walks recursively for libraries; the
/// package itself is none.
fn is_templatetag(rest: &str) -> bool {
    rest.strip_prefix("templatetags/")
        .is_some_and(|tail| is_py(tail) && tail != "__init__.py")
}

/// A module directly in `management/commands/` that Django lists as a
/// command: not a package and not starting with `_`.
fn is_command(rest: &str) -> bool {
    rest.strip_prefix("management/commands/")
        .is_some_and(|name| is_py(name) && !name.contains('/') && !name.starts_with('_'))
}

fn is_py(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|ext| ext == "py")
}

/// Whether the module binds `name` at its top level, as a definition, an
/// import that runs on load, or a star import; a module that did not parse
/// may.
fn binds(module: &ParsedModule, name: &str) -> bool {
    module.skipped
        || module.symbols.iter().any(|symbol| symbol.name == name)
        || module.imports.iter().any(|import| {
            !import.deferred
                && (import.alias.as_deref() == Some(name)
                    || (import.alias.is_none() && import.name.as_deref() == Some(name))
                    || import.name.as_deref() == Some("*"))
        })
}

/// The module's symbols with a decorator `matches` accepts.
fn decorated(module: &ParsedModule, matches: impl Fn(&str) -> bool) -> Vec<String> {
    module
        .symbols
        .iter()
        .filter(|symbol| symbol.decorators.iter().any(|decorator| matches(decorator)))
        .map(|symbol| symbol.name.clone())
        .collect()
}

fn relabel(origin: &ReferenceOrigin, label: &str) -> ReferenceOrigin {
    ReferenceOrigin {
        label: label.to_owned(),
        ..origin.clone()
    }
}
