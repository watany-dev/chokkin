//! Documentation and migration tool plugin extractors.

use std::path::Path;

use crate::config::{EntrySpec, PluginId};
use crate::manifest::literals::{assigned_value, parse_module, string_list};
use crate::sources::FileContext;

use super::context::PluginContext;
use super::types::{ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin};
use super::util::{origin_for_file, push_binary, relative_path};
use super::warnings::PluginsWarning;

/// Extract static Sphinx, `MkDocs`, and Alembic hints.
#[must_use]
pub fn extract(
    plugin: PluginId,
    ctx: &PluginContext<'_>,
) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(plugin);
    match plugin {
        PluginId::Sphinx => extract_sphinx(ctx.root.path.as_path(), &mut contrib),
        PluginId::MkDocs => extract_mkdocs(ctx.root.path.as_path(), &mut contrib),
        PluginId::Alembic => extract_alembic(ctx.root.path.as_path(), &mut contrib),
        _ => {},
    }

    let warnings = if contrib.entries.is_empty()
        && contrib.module_refs.is_empty()
        && contrib.binary_usages.is_empty()
    {
        vec![PluginsWarning::PluginNoOp { plugin }]
    } else {
        Vec::new()
    };
    (contrib, warnings)
}

fn extract_sphinx(root: &Path, contrib: &mut PluginContribution) {
    let conf = root.join("docs").join("conf.py");
    if conf.is_file() {
        push_entry(contrib, root, &conf, FileContext::Docs, "docs/conf.py");
        push_binary(
            contrib,
            "sphinx-build",
            origin_for_file(root, &conf, "docs/conf.py"),
        );
        if let Ok(contents) = std::fs::read_to_string(&conf)
            && let Some(stmts) = parse_module(&contents)
            && let Some(scan) = assigned_value(&stmts, "extensions").and_then(string_list)
        {
            let file = relative_path(root, &conf);
            for extension in scan.values {
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

fn extract_mkdocs(root: &Path, contrib: &mut PluginContribution) {
    for name in ["mkdocs.yml", "mkdocs.yaml"] {
        let path = root.join(name);
        if path.is_file() {
            push_binary(contrib, "mkdocs", origin_for_file(root, &path, name));
            return;
        }
    }
}

fn extract_alembic(root: &Path, contrib: &mut PluginContribution) {
    let env = root.join("alembic").join("env.py");
    if env.is_file() {
        push_entry(contrib, root, &env, FileContext::Dev, "alembic/env.py");
    }
    let ini = root.join("alembic.ini");
    if ini.is_file() {
        push_binary(
            contrib,
            "alembic",
            origin_for_file(root, &ini, "alembic.ini"),
        );
    }
}

fn push_entry(
    contrib: &mut PluginContribution,
    root: &Path,
    path: &Path,
    context: FileContext,
    label: &str,
) {
    let rel = relative_path(root, path);
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
