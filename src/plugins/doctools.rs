//! Documentation tool plugin extractors.

use std::path::Path;

use ruff_python_ast::{Expr, Stmt};

use crate::config::{EntrySpec, PluginId};
use crate::manifest::literals::{assigned_value, parse_module, string_list, string_value};
use crate::path_util::rel_to_root;
use crate::sources::FileContext;

use super::context::PluginContext;
use super::types::{ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin};
use super::util::{origin_for_file, push_binary};

/// Extract static Sphinx and `MkDocs` hints.
#[must_use]
pub(super) fn extract(plugin: PluginId, ctx: &PluginContext<'_>) -> PluginContribution {
    let mut contrib = PluginContribution::empty(plugin);
    match plugin {
        PluginId::Sphinx => {
            let root = ctx.root.path.as_path();
            extract_sphinx(root, root, &mut contrib);
            // Monorepos keep docs per member (`providers/*/docs/conf.py`).
            for member in &ctx.sources.layout.members {
                extract_sphinx(root, &root.join(&member.path), &mut contrib);
            }
        },
        PluginId::MkDocs => extract_mkdocs(ctx.root.path.as_path(), &mut contrib),
        _ => {},
    }

    contrib
}

fn extract_sphinx(root: &Path, dir: &Path, contrib: &mut PluginContribution) {
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
            for extension in sphinx_extensions(&stmts) {
                contrib.module_refs.push(ModuleReference {
                    module: extension,
                    origin: ReferenceOrigin {
                        file: file.clone(),
                        line: None,
                        label: "extensions".to_owned(),
                    },
                });
            }
        }
    }
}

/// The literal names in `extensions = [...]` and in later top-level
/// `extensions.append(...)`, `.extend([...])` and `+= [...]`.
fn sphinx_extensions(stmts: &[Stmt]) -> Vec<String> {
    let mut names = assigned_value(stmts, "extensions")
        .and_then(string_list)
        .map(|scan| scan.values)
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
                if !is_extensions(&method.value) {
                    continue;
                }
                match method.attr.as_str() {
                    "append" => names.extend(string_value(argument)),
                    "extend" => names.extend(
                        string_list(argument)
                            .into_iter()
                            .flat_map(|scan| scan.values),
                    ),
                    _ => {},
                }
            },
            Stmt::AugAssign(assign) if is_extensions(&assign.target) => {
                names.extend(
                    string_list(&assign.value)
                        .into_iter()
                        .flat_map(|scan| scan.values),
                );
            },
            _ => {},
        }
    }
    names
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
