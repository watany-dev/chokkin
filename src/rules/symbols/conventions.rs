//! Files and names that a tool reads by convention rather than by import (#655).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::PluginId;
use crate::parser::ParsedModule;
use crate::plugins::{DJANGO_SETTINGS_LABEL, PluginHints};

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

/// Files alembic loads by path from `script_location` (#667): `env.py` and
/// the revisions are never imported, so their names are no API surface.
pub(super) fn alembic_script_files(plugins: &PluginHints) -> HashSet<&str> {
    plugins
        .contributions
        .iter()
        .filter(|contrib| contrib.plugin == PluginId::Alembic)
        .flat_map(|contrib| &contrib.entries)
        .map(|entry| entry.spec.path.as_str())
        .collect()
}

/// alembic's revision attributes and hooks in a module under `versions/`
/// that defines both `revision` and `down_revision`, when alembic is in use.
pub(super) fn alembic_symbols(
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
/// with `__all__` hands over only the names it lists.
pub(super) fn config_star_symbols(
    plugins: &PluginHints,
    registry: &[RegistryEntry],
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
) -> Vec<SymbolId> {
    let entries = config_entry_files(plugins);
    let mut seeds = Vec::new();
    let mut star_targets: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut declares_all = HashSet::new();
    for module in modules {
        if entries.contains(module.path.as_str()) {
            seeds.extend(star_imports(module));
        }
        if let Some(name) = module_names.get(module.path.as_str()) {
            star_targets
                .entry(name.as_str())
                .or_default()
                .extend(star_imports(module));
            if !module.exports.is_empty() {
                declares_all.insert(name.as_str());
            }
        }
    }
    let reached = star_closure(&star_targets, seeds);
    registry
        .iter()
        .filter(|entry| {
            reached.contains(&entry.id.module)
                && (entry.in_all || !declares_all.contains(entry.id.module.as_str()))
        })
        .map(|entry| entry.id.clone())
        .collect()
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
}
