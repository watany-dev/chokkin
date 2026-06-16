//! Flask plugin extractor.

use std::path::Path;

use crate::config::PluginId;
use crate::parser::file_module_name;
use crate::sources::FileKind;

use super::context::PluginContext;
use super::types::{
    BinaryUsage, ModuleReference, PluginContribution, ReferenceOrigin, SymbolReference,
};
use super::util::{manifest_has_dependency, parse_module_symbol, relative_path};
use super::warnings::PluginsWarning;

/// Extract Flask app references from static configuration.
pub fn extract(ctx: &PluginContext<'_>) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(PluginId::Flask);
    let root = ctx.root.path.as_path();
    let mut found = manifest_has_dependency(ctx.manifest, "flask");

    extract_flaskenv(root, &mut contrib, &mut found);
    extract_scripts(root, &mut contrib, &mut found);
    extract_route_modules(ctx, &mut contrib, &mut found);

    let warnings = if found || !contrib.symbol_refs.is_empty() || !contrib.binary_usages.is_empty()
    {
        Vec::new()
    } else {
        vec![PluginsWarning::PluginNoOp {
            plugin: PluginId::Flask,
        }]
    };
    (contrib, warnings)
}

fn extract_flaskenv(root: &Path, contrib: &mut PluginContribution, found: &mut bool) {
    for file_name in [".flaskenv", ".env"] {
        let path = root.join(file_name);
        if !path.is_file() {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = relative_path(root, &path);
        for (line_index, line) in contents.lines().enumerate() {
            let Some(value) = env_assignment(line, "FLASK_APP") else {
                continue;
            };
            *found = true;
            push_symbol_ref(
                contrib,
                value,
                ReferenceOrigin {
                    file: rel.clone(),
                    line: u32::try_from(line_index + 1).ok(),
                    label: "FLASK_APP".to_owned(),
                },
            );
            push_binary(
                contrib,
                "flask",
                ReferenceOrigin {
                    file: rel.clone(),
                    line: u32::try_from(line_index + 1).ok(),
                    label: "FLASK_APP".to_owned(),
                },
            );
        }
    }
}

fn extract_scripts(root: &Path, contrib: &mut PluginContribution, found: &mut bool) {
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
                let Some(target) = flask_app_arg(line) else {
                    continue;
                };
                *found = true;
                let origin = ReferenceOrigin {
                    file: rel.clone(),
                    line: u32::try_from(line_index + 1).ok(),
                    label: "flask --app".to_owned(),
                };
                push_symbol_ref(contrib, target, origin.clone());
                push_binary(contrib, "flask", origin);
            }
        }
    }
}

fn extract_route_modules(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    found: &mut bool,
) {
    for file in &ctx.sources.files {
        if file.kind != FileKind::Python {
            continue;
        }
        let path = ctx.root.path.join(&file.path);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(line) = flask_route_decorator_line(&contents) else {
            continue;
        };
        let Some(module) = file_module_name(&file.path, &ctx.sources.layout) else {
            continue;
        };
        *found = true;
        contrib.module_refs.push(ModuleReference {
            module,
            origin: ReferenceOrigin {
                file: file.path.clone(),
                line: Some(line),
                label: "flask route decorator".to_owned(),
            },
        });
    }
}

fn flask_route_decorator_line(contents: &str) -> Option<u32> {
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('@') {
            continue;
        }
        let decorator = trimmed.trim_start_matches('@');
        if decorator.contains(".route(")
            || decorator.contains(".get(")
            || decorator.contains(".post(")
            || decorator.contains(".put(")
            || decorator.contains(".patch(")
            || decorator.contains(".delete(")
        {
            return u32::try_from(index + 1).ok();
        }
    }
    None
}

fn env_assignment<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return None;
    }
    let (lhs, rhs) = trimmed.split_once('=')?;
    if lhs.trim() != key {
        return None;
    }
    Some(rhs.trim().trim_matches(['"', '\'']))
}

fn flask_app_arg(line: &str) -> Option<&str> {
    let tokens: Vec<_> = line.split_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        if *token == "--app" {
            return tokens.get(index + 1).copied();
        }
        if let Some(value) = token.strip_prefix("--app=") {
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
