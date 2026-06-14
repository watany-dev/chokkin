//! Documentation and migration tool plugin extractors.

use std::path::Path;

use crate::config::{EntrySpec, PluginId};
use crate::manifest::literals::extract_python_list_assignment;
use crate::sources::FileContext;

use super::context::PluginContext;
use super::types::{
    BinaryUsage, ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin,
};
use super::util::relative_path;
use super::warnings::PluginsWarning;

/// Extract static Sphinx, MkDocs, and Alembic hints.
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
        _ => {}
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
        push_binary(contrib, root, &conf, "sphinx-build", "docs/conf.py");
        if let Ok(contents) = std::fs::read_to_string(&conf)
            && let Some(scan) = extract_python_list_assignment(&contents, "extensions")
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
            push_binary(contrib, root, &path, "mkdocs", name);
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
        push_binary(contrib, root, &ini, "alembic", "alembic.ini");
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

fn push_binary(
    contrib: &mut PluginContribution,
    root: &Path,
    path: &Path,
    binary: &str,
    label: &str,
) {
    contrib.binary_usages.push(BinaryUsage {
        binary: binary.to_owned(),
        origin: ReferenceOrigin {
            file: relative_path(root, path),
            line: None,
            label: label.to_owned(),
        },
    });
}
