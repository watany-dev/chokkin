//! Prefect plugin: the flows `prefect.yaml` deployments name by
//! `entrypoint` (#401).

use crate::config::{EntrySpec, PluginId};
use crate::path_util::rel_to_root;

use super::commands::is_dotted_identifier;
use super::config_text::{origin_at, yaml_scalar};
use super::context::PluginContext;
use super::types::{PluginContribution, PluginEntry, ReferenceOrigin, SymbolReference};
use super::util::{file_context, is_py_path, join_lexical, project_dirs};

const LABEL: &str = "entrypoint";

#[must_use]
pub(super) fn extract(ctx: &PluginContext<'_>) -> PluginContribution {
    let root = ctx.root.path.as_path();
    let mut contrib = PluginContribution::empty(PluginId::Prefect);
    let dirs = project_dirs(ctx);
    for dir in dirs {
        let path = root.join(dir).join("prefect.yaml");
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let file = rel_to_root(root, &path);
        for (index, line) in contents.lines().enumerate() {
            let Some(value) = entrypoint_value(line) else {
                continue;
            };
            let origin = origin_at(&file, index, LABEL);
            push_entrypoint(ctx, &mut contrib, dir, yaml_scalar(value), origin);
        }
    }
    contrib
}

/// The value of an `entrypoint:` key, as a mapping key or a list item's.
fn entrypoint_value(line: &str) -> Option<&str> {
    let key = line.trim_start();
    let key = key.strip_prefix("- ").map_or(key, str::trim_start);
    key.strip_prefix("entrypoint:")
        .or_else(|| key.strip_prefix("\"entrypoint\":"))
        .or_else(|| key.strip_prefix("'entrypoint':"))
}

/// `path/to/flow.py:func` is a file from the config's directory, where
/// `prefect deploy` runs; a dotted `module.func` is a module and its flow,
/// as Prefect reads an entrypoint with no `:`. A template, an alias, a
/// block scalar or any other shape is not read, so it is a gap. A file that
/// does not exist hides no project file, and `null` (as `prefect init`
/// writes it) names none, so neither is an entry or a gap.
fn push_entrypoint(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    dir: &str,
    value: &str,
    origin: ReferenceOrigin,
) {
    if ["", "~", "null", "Null", "NULL"].contains(&value) {
        return;
    }
    if let Some((path, func)) = value.rsplit_once(':')
        && is_py_path(path)
        && !path.contains(['{', '$'])
        && is_dotted_identifier(func)
    {
        let Some(path) = join_lexical(dir, path) else {
            return contrib.push_gap(value, origin);
        };
        if !ctx.root.path.join(&path).is_file() {
            return;
        }
        let context = file_context(ctx, &path);
        contrib.entries.push(PluginEntry {
            spec: EntrySpec {
                path,
                symbol: Some(func.to_owned()),
            },
            context,
            origin,
        });
        return;
    }
    if !value.contains(':')
        && let Some((module, func)) = value.rsplit_once('.')
        && is_dotted_identifier(module)
        && is_dotted_identifier(func)
    {
        contrib.symbol_refs.push(SymbolReference {
            module: module.to_owned(),
            symbol: func.to_owned(),
            origin,
        });
        return;
    }
    contrib.push_gap(value, origin);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entrypoint_value_reads_keys_and_list_items() {
        assert_eq!(
            entrypoint_value("    entrypoint: flows/etl.py:run"),
            Some(" flows/etl.py:run")
        );
        assert_eq!(entrypoint_value("  - entrypoint: a.b"), Some(" a.b"));
        assert_eq!(entrypoint_value("  \"entrypoint\": x"), Some(" x"));
        assert_eq!(entrypoint_value("  entrypoints: x"), None);
        assert_eq!(entrypoint_value("# entrypoint: x"), None);
    }
}
