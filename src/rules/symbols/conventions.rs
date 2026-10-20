//! Files and names that a tool reads by convention rather than by import (#655).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::PluginId;
use crate::entry::{EntryOrigin, EntryPlan};
use crate::parser::{ImportContext, ParsedModule, SymbolKind};
use crate::plugins::{DJANGO_SETTINGS_LABEL, PluginHints};
use crate::reachability::ReachabilityReport;

use super::exports::ReExport;
use super::graph::{RegistryEntry, SymbolId};
use super::public::{star_closure, star_imports};

/// protoc output: generated, never edited by hand (ruff excludes it too).
const GENERATED_SUFFIXES: &[&str] = &["_pb2.py", "_pb2_grpc.py"];

/// Module attributes alembic reads from a revision file.
const ALEMBIC_NAMES: &[&str] = &[
    "revision",
    "down_revision",
    "branch_labels",
    "depends_on",
    "upgrade",
    "downgrade",
];

/// The PEP 517 and PEP 660 hooks a frontend calls on a build backend.
const BUILD_BACKEND_HOOKS: &[&str] = &[
    "build_wheel",
    "build_sdist",
    "build_editable",
    "get_requires_for_build_wheel",
    "get_requires_for_build_sdist",
    "get_requires_for_build_editable",
    "prepare_metadata_for_build_wheel",
    "prepare_metadata_for_build_editable",
];

/// Whether a file's symbols are no API surface: protoc output, or a
/// transformers `modular_x.py` that generates the sibling `modeling_x.py`.
pub(super) fn is_codegen_file(path: &str, files: &HashSet<&str>) -> bool {
    let Some(name) = Path::new(path).file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if GENERATED_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return true;
    }
    // Keep the `/`-separated form `files` uses; `Path::with_file_name` would
    // join with `\` on Windows.
    let dir = &path[..path.len() - name.len()];
    name.strip_prefix("modular_")
        .is_some_and(|stem| files.contains(format!("{dir}modeling_{stem}").as_str()))
}

/// Files a framework reads the names of itself (#667, #401): nothing
/// imports them for those names, so they are no API surface.
pub(super) fn scanned_files(plugins: &PluginHints) -> HashSet<&str> {
    plugins
        .contributions
        .iter()
        .flat_map(|contrib| &contrib.scanned_files)
        .map(String::as_str)
        .collect()
}

/// Names a framework reads by convention from the modules it loads by path:
/// alembic's revisions and Django's migrations.
pub(super) fn convention_symbols(
    plugins: &PluginHints,
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
) -> Vec<SymbolId> {
    let mut symbols = alembic_symbols(plugins, modules, module_names);
    symbols.extend(django_migration_symbols(plugins, modules, module_names));
    symbols
}

/// alembic's revision attributes and hooks in a module under `versions/`
/// that defines both `revision` and `down_revision`, when alembic is in use.
fn alembic_symbols(
    plugins: &PluginHints,
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
) -> Vec<SymbolId> {
    let enabled = plugins
        .contributions
        .iter()
        .any(|contrib| contrib.plugin == PluginId::Alembic);
    if !enabled {
        return Vec::new();
    }
    modules
        .iter()
        .filter(|module| is_alembic_revision(module))
        .filter_map(|module| module_names.get(module.path.as_str()))
        .flat_map(|owner| {
            ALEMBIC_NAMES
                .iter()
                .map(move |name| SymbolId::new(owner.clone(), *name))
        })
        .collect()
}

fn is_alembic_revision(module: &ParsedModule) -> bool {
    let in_versions = Path::new(&module.path)
        .components()
        .any(|part| part.as_os_str() == "versions");
    let defines = |name: &str| module.symbols.iter().any(|symbol| symbol.name == name);
    in_versions && defines("revision") && defines("down_revision")
}

/// `Migration` in a module directly under a `migrations` directory, when
/// Django is in use (#926): Django's migration loader reads it by name from
/// each module of an app's `migrations` package. A package that
/// `MIGRATION_MODULES` renames is not read, so its `Migration` stays CHK006.
fn django_migration_symbols(
    plugins: &PluginHints,
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
) -> Vec<SymbolId> {
    let enabled = plugins
        .contributions
        .iter()
        .any(|contrib| contrib.plugin == PluginId::Django);
    if !enabled {
        return Vec::new();
    }
    modules
        .iter()
        .filter(|module| is_django_migration(module))
        .filter_map(|module| module_names.get(module.path.as_str()))
        .map(|owner| SymbolId::new(owner.clone(), "Migration"))
        .collect()
}

fn is_django_migration(module: &ParsedModule) -> bool {
    let path = Path::new(&module.path);
    let in_migrations = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "migrations");
    in_migrations
        && path.file_stem().is_some_and(|stem| stem != "__init__")
        && module
            .symbols
            .iter()
            .any(|symbol| symbol.name == "Migration")
}

/// Entries whose module namespace the host reads as settings: Sphinx's
/// `docs/conf.py` and Django's settings module.
fn config_entry_files(plugins: &PluginHints) -> HashSet<&str> {
    plugins
        .contributions
        .iter()
        .flat_map(|contrib| {
            contrib
                .entries
                .iter()
                .filter(move |entry| match contrib.plugin {
                    PluginId::Sphinx => true,
                    PluginId::Django => entry.origin.label == DJANGO_SETTINGS_LABEL,
                    _ => false,
                })
        })
        .map(|entry| entry.spec.path.as_str())
        .collect()
}

/// Names a config entry pulls into its namespace with `from m import *`,
/// through chained star imports (#729): the host reads them as settings
/// (`html_theme`, `INSTALLED_APPS`), so no importer ever names them. A module
/// hands over the names `Exports::may_hand_on` allows.
pub(super) fn config_star_symbols(
    plugins: &PluginHints,
    registry: &[RegistryEntry],
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
) -> Vec<SymbolId> {
    let entries = config_entry_files(plugins);
    let mut seeds = Vec::new();
    let mut star_targets: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut exports = HashMap::new();
    for module in modules {
        if entries.contains(module.path.as_str()) {
            seeds.extend(star_imports(module));
        }
        if let Some(name) = module_names.get(module.path.as_str()) {
            star_targets
                .entry(name.as_str())
                .or_default()
                .extend(star_imports(module));
            exports.insert(name.as_str(), &module.exports);
        }
    }
    let reached = star_closure(&star_targets, seeds);
    registry
        .iter()
        .filter(|entry| {
            reached.contains(&entry.id.module)
                && exports
                    .get(entry.id.module.as_str())
                    .is_none_or(|all| all.may_hand_on(&entry.id.name))
        })
        .map(|entry| entry.id.clone())
        .collect()
}

/// Functions a host calls by name (#777): a `pytest_*` hook in a module pytest
/// loads as a plugin, the `setup` of a Sphinx extension, and the hooks of a
/// build backend (#782), defined there or re-exported by a package (#873).
/// pluggy hooks are marked by their decorator
/// (`external::is_registration_decorator`).
#[allow(clippy::too_many_arguments)]
pub(super) fn host_hook_symbols(
    registry: &[RegistryEntry],
    reexports: &[ReExport],
    modules: &[&ParsedModule],
    plan: &EntryPlan,
    plugins: &PluginHints,
    reachability: &ReachabilityReport,
) -> Vec<SymbolId> {
    let pytest_plugins: HashSet<&str> = modules
        .iter()
        .flat_map(|module| &module.pytest_plugins)
        .map(|plugin| plugin.module.as_str())
        .chain(plugin_module_refs(plugins, PluginId::Pytest))
        .collect();
    let pytest_files: HashSet<&str> = plan
        .roots
        .iter()
        .filter(|root| {
            root.origins.iter().any(
                |origin| matches!(origin, EntryOrigin::Manifest { group, .. } if group == "pytest11"),
            )
        })
        .map(|root| root.path.as_str())
        .chain(
            modules
                .iter()
                .filter(|module| imports_pytest(module))
                .map(|module| module.path.as_str()),
        )
        .collect();
    // The file an extension name loads, not every module ending with it: a
    // name found on a `sys.path` directory drops its package (#779).
    let extension_files: HashSet<&str> = plugins
        .contributions
        .iter()
        .filter(|contrib| contrib.plugin == PluginId::Sphinx)
        .flat_map(|contrib| &contrib.module_refs)
        .filter_map(|reference| {
            let key = (reference.origin.file.clone(), reference.module.clone());
            reachability.plugin_ref_files.get(&key)
        })
        .map(String::as_str)
        .collect();
    let backend_files: HashSet<&str> = plan
        .roots
        .iter()
        .filter(|root| {
            root.origins.iter().any(|origin| match origin {
                EntryOrigin::BuildBackend => true,
                EntryOrigin::Manifest { group, .. } => group == "build-system.backend-path",
                _ => false,
            })
        })
        .map(|root| root.path.as_str())
        .collect();
    let is_hook = |name: &str, module: &str, path: &str| match name {
        name if name.starts_with("pytest_") => {
            pytest_plugins.contains(module) || pytest_files.contains(path)
        },
        "setup" => extension_files.contains(path),
        name if BUILD_BACKEND_HOOKS.contains(&name) => backend_files.contains(path),
        _ => false,
    };
    // A package can hand the host a hook it imports from a submodule
    // (`from .core import setup`): the name the host reads is the package's
    // (#873).
    let reexported = reexports
        .iter()
        .filter(|reexport| is_hook(&reexport.name, &reexport.package_module, &reexport.path))
        .map(|reexport| SymbolId::new(reexport.package_module.clone(), reexport.name.clone()));
    registry
        .iter()
        .filter(|entry| entry.def.kind == SymbolKind::Function)
        .filter(|entry| is_hook(&entry.id.name, &entry.id.module, &entry.path))
        .map(|entry| entry.id.clone())
        .chain(reexported)
        .collect()
}

fn plugin_module_refs(plugins: &PluginHints, plugin: PluginId) -> impl Iterator<Item = &str> {
    plugins
        .contributions
        .iter()
        .filter(move |contrib| contrib.plugin == plugin)
        .flat_map(|contrib| &contrib.module_refs)
        .map(|reference| reference.module.as_str())
}

/// pytest only loads a plugin module it is told about; one that imports
/// pytest at run time is written for it, as an opt-in `-p` plugin may be
/// (mlflow).
fn imports_pytest(module: &ParsedModule) -> bool {
    module.imports.iter().any(|import| {
        !import.deferred
            && import.context != ImportContext::Type
            && matches!(import.module.split('.').next(), Some("pytest" | "_pytest"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{SymbolDef, SymbolKind};

    #[test]
    fn protoc_output_and_modular_sources_are_codegen() {
        let files: HashSet<&str> = [
            "src/t/models/bert/modular_bert.py",
            "src/t/models/bert/modeling_bert.py",
            "src/t/models/gpt/modular_gpt.py",
        ]
        .into_iter()
        .collect();
        for path in [
            "pkg/protos/service_pb2.py",
            "pkg/protos/service_pb2_grpc.py",
            "src/t/models/bert/modular_bert.py",
        ] {
            assert!(is_codegen_file(path, &files), "{path}");
        }
        for path in [
            "pkg/protos/service.py",
            "pkg/pb2.py",
            "src/t/models/gpt/modular_gpt.py",
            "src/t/models/bert/modeling_bert.py",
        ] {
            assert!(!is_codegen_file(path, &files), "{path}");
        }
    }

    fn module(path: &str, names: &[&str]) -> ParsedModule {
        ParsedModule {
            path: path.to_owned(),
            symbols: names
                .iter()
                .map(|name| SymbolDef {
                    name: (*name).to_owned(),
                    kind: SymbolKind::Variable,
                    line: 1,
                    is_public: true,
                    decorators: Vec::new(),
                    in_type_checking: false,
                    used_in_module: false,
                })
                .collect(),
            ..ParsedModule::default()
        }
    }

    #[test]
    fn alembic_revision_needs_versions_dir_and_both_revision_names() {
        let both = ["revision", "down_revision", "upgrade"];
        assert!(is_alembic_revision(&module("db/versions/a1.py", &both)));
        assert!(is_alembic_revision(&module("db/versions/sub/a1.py", &both)));
        assert!(!is_alembic_revision(&module("db/revisions/a1.py", &both)));
        assert!(!is_alembic_revision(&module(
            "db/versions/a1.py",
            &["revision", "upgrade"]
        )));
        assert!(!is_alembic_revision(&module(
            "db/versions/a1.py",
            &["down_revision", "upgrade"]
        )));
    }

    #[test]
    fn django_migration_is_a_module_directly_under_migrations() {
        let defines = ["Migration"];
        assert!(is_django_migration(&module(
            "polls/migrations/0001_initial.py",
            &defines
        )));
        assert!(!is_django_migration(&module(
            "polls/migrations/__init__.py",
            &defines
        )));
        assert!(!is_django_migration(&module(
            "polls/migrations/sub/0001_initial.py",
            &defines
        )));
        assert!(!is_django_migration(&module("polls/models.py", &defines)));
        assert!(!is_django_migration(&module(
            "polls/migrations/0001_initial.py",
            &["operations"]
        )));
    }
}
