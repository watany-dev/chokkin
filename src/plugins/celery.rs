//! Celery plugin extractor.

use std::path::Path;

use crate::config::PluginId;
use crate::path_util::rel_to_root;

use super::context::PluginContext;
use super::types::{PluginContribution, ReferenceOrigin};
use super::util::{
    decorator_suffix, push_app_ref, push_binary, push_decorated_modules, push_used_symbol,
    read_pyproject_table, toml_origin,
};

/// Attributes Celery looks up in the module `-A` names.
const APP_NAMES: [&str; 2] = ["app", "celery"];

/// Extract Celery app references from static command configuration.
pub(super) fn extract(ctx: &PluginContext<'_>) -> PluginContribution {
    let mut contrib = PluginContribution::empty(PluginId::Celery);
    let root = ctx.root.path.as_path();

    extract_pyproject_scripts(root, &mut contrib);
    extract_shell_scripts(root, &mut contrib);
    push_decorated_modules(
        ctx,
        &mut contrib,
        is_task_decorator,
        "celery task decorator",
    );

    contrib
}

fn extract_pyproject_scripts(root: &Path, contrib: &mut PluginContribution) {
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
        let origin = toml_origin(root, &path, format!("project.scripts.{name}"));
        push_celery_app(contrib, app, &origin);
        push_binary(contrib, "celery", origin);
    }
}

fn extract_shell_scripts(root: &Path, contrib: &mut PluginContribution) {
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
            let rel = rel_to_root(root, &path);
            for (line_index, line) in contents.lines().enumerate() {
                let Some(app) = celery_app_arg(line) else {
                    continue;
                };
                let origin = ReferenceOrigin {
                    file: rel.clone(),
                    line: u32::try_from(line_index + 1).ok(),
                    label: "celery app".to_owned(),
                };
                push_celery_app(contrib, app, &origin);
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

/// `-A pkg.mod` is the module Celery searches for [`APP_NAMES`], or, since
/// Celery resolves the value as `pkg` plus an attribute first, the attribute
/// `mod` of `pkg`. A package without those attributes makes Celery import its
/// `celery` submodule (#865); whether the module is a package is unknown
/// here, so `pkg.mod.celery` is always referenced, and a module that does not
/// exist adds nothing.
fn push_celery_app(contrib: &mut PluginContribution, value: &str, origin: &ReferenceOrigin) {
    let Some(module) = push_app_ref(contrib, value, origin, &APP_NAMES) else {
        return;
    };
    if let Some((package, attribute)) = module.rsplit_once('.') {
        push_used_symbol(contrib, package, attribute, origin);
    }
    push_app_ref(contrib, &format!("{module}.celery"), origin, &APP_NAMES);
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
