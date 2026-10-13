//! Documentation tool plugin extractors.

use std::collections::HashMap;
use std::path::Path;

use ruff_python_ast::{Expr, Stmt};

use crate::config::{EntrySpec, PluginId};
use crate::manifest::literals::{assigned_value, parse_module, string_list, string_value};
use crate::manifest::{LoadedManifest, normalize_distribution_name};
use crate::path_util::rel_to_root;
use crate::sources::{DiscoveredSources, FileContext, path_to_module};

use super::context::PluginContext;
use super::types::{
    ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin, SysPathImport,
};
use super::util::{origin_for_file, push_binary};

/// Extract static Sphinx and `MkDocs` hints.
#[must_use]
pub(super) fn extract(plugin: PluginId, ctx: &PluginContext<'_>) -> PluginContribution {
    let mut contrib = PluginContribution::empty(plugin);
    match plugin {
        PluginId::Sphinx => {
            let root = ctx.root.path.as_path();
            extract_sphinx(ctx, root, &mut contrib);
            // Monorepos keep docs per member (`providers/*/docs/conf.py`).
            for member in &ctx.sources.layout.members {
                extract_sphinx(ctx, &root.join(&member.path), &mut contrib);
            }
        },
        PluginId::MkDocs => extract_mkdocs(ctx.root.path.as_path(), &mut contrib),
        _ => {},
    }

    contrib
}

fn extract_sphinx(ctx: &PluginContext<'_>, dir: &Path, contrib: &mut PluginContribution) {
    let root = ctx.root.path.as_path();
    let conf = dir.join("docs").join("conf.py");
    if conf.is_file() {
        push_entry(contrib, root, &conf, FileContext::Docs, "docs/conf.py");
        push_binary(
            contrib,
            "sphinx-build",
            origin_for_file(root, &conf, "docs/conf.py"),
        );
        if let Ok(contents) = std::fs::read_to_string(&conf)
            && let Some(stmts) = parse_module(&contents)
        {
            let file = rel_to_root(root, &conf);
            let imports = ConfImports::new(&stmts, ctx.sources);
            // Extensions often live in a directory an imported constants
            // module appends to `sys.path` (airflow's `conf_constants`).
            for imported in &imports.files {
                contrib.sys_path_imports.push(SysPathImport {
                    importer: file.clone(),
                    imported: imported.clone(),
                });
            }
            let theme = declared_theme(&stmts, ctx.manifest);
            let refs = sphinx_extensions(&stmts, &|name| imports.strings(root, name))
                .into_iter()
                .map(|module| (module, "extensions"))
                .chain(theme.map(|module| (module, "html_theme")));
            for (module, label) in refs {
                contrib.module_refs.push(ModuleReference {
                    module,
                    origin: ReferenceOrigin {
                        file: file.clone(),
                        line: None,
                        label: label.to_owned(),
                    },
                });
            }
        }
    }
}

/// First-party modules a `conf.py` imports at top level.
#[derive(Default)]
struct ConfImports {
    /// Local name → (module file, name in it) of `from m import name [as local]`.
    names: HashMap<String, (String, String)>,
    /// Root-relative files of the imported modules.
    files: Vec<String>,
}

impl ConfImports {
    fn new(stmts: &[Stmt], sources: &DiscoveredSources) -> Self {
        let mut modules: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for stmt in stmts {
            match stmt {
                Stmt::ImportFrom(from) if from.level == 0 => {
                    let Some(module) = &from.module else {
                        continue;
                    };
                    let names = modules.entry(module.to_string()).or_default();
                    for alias in &from.names {
                        let local = alias.asname.as_ref().unwrap_or(&alias.name);
                        names.push((local.to_string(), alias.name.to_string()));
                    }
                },
                Stmt::Import(import) => {
                    for alias in &import.names {
                        modules.entry(alias.name.to_string()).or_default();
                    }
                },
                _ => {},
            }
        }
        let mut imports = Self::default();
        if modules.is_empty() {
            return imports;
        }
        for file in &sources.files {
            let Some(names) = path_to_module(&file.path, &sources.layout)
                .and_then(|module| modules.remove(&module))
            else {
                continue;
            };
            for (local, name) in names {
                imports.names.insert(local, (file.path.clone(), name));
            }
            imports.files.push(file.path.clone());
        }
        imports
    }

    /// The strings the imported module assigns to the name `conf.py` binds
    /// as `local` (`BASIC_SPHINX_EXTENSIONS = [...]`); one level only.
    fn strings(&self, root: &Path, local: &str) -> Vec<String> {
        let Some((path, name)) = self.names.get(local) else {
            return Vec::new();
        };
        let Some(stmts) = std::fs::read_to_string(root.join(path))
            .ok()
            .and_then(|contents| parse_module(&contents))
        else {
            return Vec::new();
        };
        assigned_value(&stmts, name)
            .map(|value| literal_strings(value, &|_| Vec::new()))
            .unwrap_or_default()
    }
}

/// `html_theme` when a declared distribution has its name (`furo`,
/// `sphinx_rtd_theme`). A theme is registered by name rather than imported,
/// and Sphinx ships its own (`alabaster`, `classic`), so an undeclared name
/// is no module to resolve (#809).
fn declared_theme(stmts: &[Stmt], manifest: &LoadedManifest) -> Option<String> {
    let theme = assigned_value(stmts, "html_theme").and_then(string_value)?;
    let distribution = normalize_distribution_name(&theme);
    manifest
        .dependencies
        .iter()
        .any(|dep| normalize_distribution_name(&dep.name) == distribution)
        .then_some(theme)
}

/// The names in `extensions = [...]` and in later top-level
/// `extensions.append(...)`, `.extend([...])` and `+= [...]`. A bare name
/// instead of a literal goes through `constant` (`extensions =
/// BASIC_SPHINX_EXTENSIONS` imported from a constants module, #779).
fn sphinx_extensions(stmts: &[Stmt], constant: &dyn Fn(&str) -> Vec<String>) -> Vec<String> {
    let mut names = assigned_value(stmts, "extensions")
        .map(|value| literal_strings(value, constant))
        .unwrap_or_default();
    let is_extensions =
        |expr: &Expr| matches!(expr, Expr::Name(name) if name.id.as_str() == "extensions");
    for stmt in stmts {
        match stmt {
            Stmt::Expr(stmt) => {
                let Expr::Call(call) = &*stmt.value else {
                    continue;
                };
                let Expr::Attribute(method) = &*call.func else {
                    continue;
                };
                let [argument] = &*call.arguments.args else {
                    continue;
                };
                if is_extensions(&method.value)
                    && matches!(method.attr.as_str(), "append" | "extend")
                {
                    names.extend(literal_strings(argument, constant));
                }
            },
            Stmt::AugAssign(assign) if is_extensions(&assign.target) => {
                names.extend(literal_strings(&assign.value, constant));
            },
            _ => {},
        }
    }
    names
}

/// The strings of a string or list literal, or of the constant a name binds.
fn literal_strings(expr: &Expr, constant: &dyn Fn(&str) -> Vec<String>) -> Vec<String> {
    match expr {
        Expr::Name(name) => constant(name.id.as_str()),
        _ => string_value(expr)
            .map(|value| vec![value])
            .or_else(|| string_list(expr).map(|scan| scan.values))
            .unwrap_or_default(),
    }
}

fn extract_mkdocs(root: &Path, contrib: &mut PluginContribution) {
    for name in ["mkdocs.yml", "mkdocs.yaml"] {
        let path = root.join(name);
        if path.is_file() {
            push_binary(contrib, "mkdocs", origin_for_file(root, &path, name));
            return;
        }
    }
}

fn push_entry(
    contrib: &mut PluginContribution,
    root: &Path,
    path: &Path,
    context: FileContext,
    label: &str,
) {
    let rel = rel_to_root(root, path);
    contrib.entries.push(PluginEntry {
        spec: EntrySpec {
            path: rel.clone(),
            symbol: None,
        },
        context,
        origin: ReferenceOrigin {
            file: rel,
            line: None,
            label: label.to_owned(),
        },
    });
}
