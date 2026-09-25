//! Celery plugin extractor.

use std::path::Path;

use crate::config::PluginId;

use super::context::PluginContext;
use super::types::{PluginContribution, ReferenceOrigin};
use super::util::{
    decorator_suffix, manifest_has_dependency, push_binary, push_decorated_modules,
    push_symbol_ref, read_pyproject_table, relative_path,
};
use super::warnings::PluginsWarning;

/// Extract Celery app references from static command configuration.
pub fn extract(ctx: &PluginContext<'_>) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(PluginId::Celery);
    let root = ctx.root.path.as_path();
    let mut found = manifest_has_dependency(ctx.manifest, "celery");

    extract_pyproject_scripts(root, &mut contrib, &mut found);
    extract_shell_scripts(root, &mut contrib, &mut found);
    found |= push_decorated_modules(
        ctx,
        &mut contrib,
        is_task_decorator,
        "celery task decorator",
    );

    let warnings = if found || !contrib.symbol_refs.is_empty() || !contrib.binary_usages.is_empty()
    {
        Vec::new()
    } else {
        vec![PluginsWarning::PluginNoOp {
            plugin: PluginId::Celery,
        }]
    };
    (contrib, warnings)
}

fn extract_pyproject_scripts(root: &Path, contrib: &mut PluginContribution, found: &mut bool) {
    let path = root.join("pyproject.toml");
    if !path.is_file() {
        return;
    }
    let Ok(table) = read_pyproject_table(&path) else {
        return;
    };
    let Some(scripts) = table
        .get("project")
        .and_then(toml::Value::as_table)
        .and_then(|project| project.get("scripts"))
        .and_then(toml::Value::as_table)
    else {
        return;
    };
    for (name, target) in scripts {
        let Some(command) = target.as_str() else {
            continue;
        };
        let Some(app) = celery_app_arg(command) else {
            continue;
        };
        *found = true;
        let origin = ReferenceOrigin {
            file: "pyproject.toml".to_owned(),
            line: None,
            label: format!("project.scripts.{name}"),
        };
        push_symbol_ref(contrib, app, origin.clone());
        push_binary(contrib, "celery", origin);
    }
}

fn extract_shell_scripts(root: &Path, contrib: &mut PluginContribution, found: &mut bool) {
    for dir_name in ["scripts", "bin"] {
        let dir = root.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = relative_path(root, &path);
            for (line_index, line) in contents.lines().enumerate() {
                let Some(app) = celery_app_arg(line) else {
                    continue;
                };
                *found = true;
                let origin = ReferenceOrigin {
                    file: rel.clone(),
                    line: u32::try_from(line_index + 1).ok(),
                    label: "celery app".to_owned(),
                };
                push_symbol_ref(contrib, app, origin.clone());
                push_binary(contrib, "celery", origin);
            }
        }
    }
}

/// Task decorator test shared by the parse path and its syntax-error text
/// fallback: bare `@shared_task` or any `@<receiver>.task` /
/// `@<receiver>.shared_task`, called or not.
fn is_task_decorator(name: &str, _is_call: bool) -> bool {
    let (receiver, suffix) = decorator_suffix(name);
    match suffix {
        "shared_task" => true,
        "task" => receiver.is_some(),
        _ => false,
    }
}

fn celery_app_arg(line: &str) -> Option<&str> {
    let tokens: Vec<_> = line.split_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        if *token == "-A" || *token == "--app" {
            return tokens.get(index + 1).copied();
        }
        if let Some(value) = token
            .strip_prefix("-A")
            .filter(|value| !value.is_empty())
            .or_else(|| token.strip_prefix("--app="))
        {
            return Some(value);
        }
    }
    None
}
