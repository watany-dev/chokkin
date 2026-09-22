//! Celery plugin extractor.

use std::path::Path;

use crate::config::PluginId;
use crate::sources::{FileKind, path_to_module};

use super::context::PluginContext;
use super::types::{
    BinaryUsage, ModuleReference, PluginContribution, ReferenceOrigin, SymbolReference,
};
use super::util::{
    decorator_line, decorator_suffix, manifest_has_dependency, parse_module_symbol,
    read_pyproject_table, relative_path,
};
use super::warnings::PluginsWarning;

/// Extract Celery app references from static command configuration.
pub fn extract(ctx: &PluginContext<'_>) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(PluginId::Celery);
    let root = ctx.root.path.as_path();
    let mut found = manifest_has_dependency(ctx.manifest, "celery");

    extract_pyproject_scripts(root, &mut contrib, &mut found);
    extract_shell_scripts(root, &mut contrib, &mut found);
    extract_task_modules(ctx, &mut contrib, &mut found);

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

fn extract_task_modules(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    found: &mut bool,
) {
    if let Some(parse) = ctx.parse {
        for module in &parse.modules {
            let Some(line) = decorator_line(module, is_task_decorator) else {
                continue;
            };
            push_task_module(ctx, contrib, found, &module.path, line);
        }
        return;
    }

    // Standalone callers run before step 6, so there is nothing to reuse.
    for file in &ctx.sources.files {
        if file.kind != FileKind::Python {
            continue;
        }
        let path = ctx.root.path.join(&file.path);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(line) = celery_task_decorator_line(&contents) else {
            continue;
        };
        push_task_module(ctx, contrib, found, &file.path, line);
    }
}

fn push_task_module(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    found: &mut bool,
    file: &str,
    line: u32,
) {
    let Some(module) = path_to_module(file, &ctx.sources.layout) else {
        return;
    };
    *found = true;
    contrib.module_refs.push(ModuleReference {
        module,
        origin: ReferenceOrigin {
            file: file.to_owned(),
            line: Some(line),
            label: "celery task decorator".to_owned(),
        },
    });
}

/// Mirrors [`celery_task_decorator_line`]: bare `@shared_task` or any
/// `@<receiver>.task` / `@<receiver>.shared_task`.
fn is_task_decorator(name: &str) -> bool {
    let (receiver, suffix) = decorator_suffix(name);
    match suffix {
        "shared_task" => true,
        "task" => receiver.is_some(),
        _ => false,
    }
}

fn celery_task_decorator_line(contents: &str) -> Option<u32> {
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('@') {
            continue;
        }
        let decorator = trimmed.trim_start_matches('@');
        if decorator.starts_with("shared_task")
            || decorator.starts_with("celery_app.task")
            || decorator.starts_with("app.task")
            || decorator.contains(".task(")
        {
            return u32::try_from(index + 1).ok();
        }
    }
    None
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

fn push_symbol_ref(contrib: &mut PluginContribution, value: &str, origin: ReferenceOrigin) {
    if let Some((module, symbol)) = parse_module_symbol(value) {
        contrib.symbol_refs.push(SymbolReference {
            module,
            symbol,
            origin,
        });
    }
}

fn push_binary(contrib: &mut PluginContribution, binary: &str, origin: ReferenceOrigin) {
    contrib.binary_usages.push(BinaryUsage {
        binary: binary.to_owned(),
        origin,
    });
}
