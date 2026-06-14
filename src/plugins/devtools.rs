//! Dev-tool plugin extractors for tox, nox, and pre-commit.

use std::path::Path;

use crate::config::PluginId;

use super::context::PluginContext;
use super::types::{BinaryUsage, PluginContribution, ReferenceOrigin};
use super::util::{read_pyproject_table, relative_path};
use super::warnings::PluginsWarning;

/// Extract static dev-tool config hints.
#[must_use]
pub fn extract(
    plugin: PluginId,
    ctx: &PluginContext<'_>,
) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(plugin);
    match plugin {
        PluginId::Tox => extract_file_or_tool_table(
            ctx.root.path.as_path(),
            &mut contrib,
            "tox",
            &[("tox.ini", "tox.ini")],
            &["tox"],
        ),
        PluginId::Nox => extract_file_or_tool_table(
            ctx.root.path.as_path(),
            &mut contrib,
            "nox",
            &[("noxfile.py", "noxfile.py")],
            &["nox"],
        ),
        PluginId::PreCommit => extract_file_or_tool_table(
            ctx.root.path.as_path(),
            &mut contrib,
            "pre-commit",
            &[(".pre-commit-config.yaml", ".pre-commit-config.yaml")],
            &["pre-commit", "pre_commit"],
        ),
        _ => {}
    }

    let warnings = if contrib.binary_usages.is_empty() {
        vec![PluginsWarning::PluginNoOp { plugin }]
    } else {
        Vec::new()
    };
    (contrib, warnings)
}

fn extract_file_or_tool_table(
    root: &Path,
    contrib: &mut PluginContribution,
    binary: &str,
    files: &[(&str, &str)],
    tool_keys: &[&str],
) {
    for (file_name, label) in files {
        let path = root.join(file_name);
        if !path.is_file() {
            continue;
        }
        push_binary(
            contrib,
            binary,
            ReferenceOrigin {
                file: relative_path(root, &path),
                line: None,
                label: (*label).to_owned(),
            },
        );
        return;
    }

    let pyproject = root.join("pyproject.toml");
    if !pyproject.is_file() {
        return;
    }
    let Ok(table) = read_pyproject_table(&pyproject) else {
        return;
    };
    let Some(tool) = table.get("tool").and_then(toml::Value::as_table) else {
        return;
    };
    for key in tool_keys {
        if tool.contains_key(*key) {
            push_binary(
                contrib,
                binary,
                ReferenceOrigin {
                    file: relative_path(root, &pyproject),
                    line: None,
                    label: format!("tool.{key}"),
                },
            );
            return;
        }
    }
}

fn push_binary(contrib: &mut PluginContribution, binary: &str, origin: ReferenceOrigin) {
    contrib.binary_usages.push(BinaryUsage {
        binary: binary.to_owned(),
        origin,
    });
}
