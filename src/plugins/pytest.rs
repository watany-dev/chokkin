//! pytest plugin extractor.

#![allow(clippy::too_many_lines)]

use crate::config::PluginId;
use crate::sources::FileContext;

use super::context::PluginContext;
use super::types::{
    BinaryUsage, ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin,
};
use super::util::{
    match_paths_against_globs, origin_for_file, parse_path_list, pytest_ini_options_from_pyproject,
    pytest_test_globs, read_ini_section, read_pyproject_table, relative_path,
};
use super::warnings::PluginsWarning;

/// Extract pytest-related plugin hints.
pub fn extract(ctx: &PluginContext<'_>) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(PluginId::Pytest);
    let mut warnings = Vec::new();
    let root = ctx.root.path.as_path();

    let pyproject_path = root.join("pyproject.toml");
    let mut testpaths = Vec::new();
    let mut python_files = Vec::new();
    let mut has_explicit_config = false;
    let mut config_origin: Option<ReferenceOrigin> = None;
    let mut pyproject_table: Option<toml::Table> = None;

    if pyproject_path.is_file() {
        match read_pyproject_table(&pyproject_path) {
            Ok(table) => {
                if let Some(options) = pytest_ini_options_from_pyproject(&table) {
                    has_explicit_config = true;
                    config_origin = Some(origin_for_file(
                        root,
                        &pyproject_path,
                        "tool.pytest.ini_options",
                    ));
                    testpaths = str_list(options, "testpaths");
                    python_files = str_list(options, "python_files");
                }
                pyproject_table = Some(table);
            },
            Err(error) => {
                warnings.push(PluginsWarning::PluginExtractFailed {
                    plugin: PluginId::Pytest,
                    detail: error.to_string(),
                });
                return (contrib, warnings);
            },
        }
    }

    // Order is precedence. Only `pytest.ini` warns when empty, because pytest
    // selects it as the config file even without a `[pytest]` section.
    for (file, section_name, label, warn_if_empty) in [
        ("pytest.ini", "pytest", "pytest.ini [pytest]", true),
        ("setup.cfg", "tool:pytest", "setup.cfg [tool:pytest]", false),
    ] {
        if config_origin.is_some() {
            break;
        }
        let path = root.join(file);
        if !path.is_file() {
            continue;
        }
        match read_ini_section(&path, section_name) {
            Ok(section) if section.is_empty() => {
                if warn_if_empty {
                    warnings.push(PluginsWarning::PytestConfigUnreadable {
                        path: relative_path(root, &path),
                    });
                }
            },
            Ok(section) => {
                has_explicit_config = true;
                config_origin = Some(origin_for_file(root, &path, label));
                if let Some(value) = section.get("testpaths") {
                    testpaths = parse_path_list(value);
                }
                if let Some(value) = section.get("python_files") {
                    python_files = parse_path_list(value);
                }
            },
            Err(error) => {
                warnings.push(PluginsWarning::PluginExtractFailed {
                    plugin: PluginId::Pytest,
                    detail: error.to_string(),
                });
            },
        }
    }

    let origin = config_origin.take().unwrap_or_else(|| ReferenceOrigin {
        file: "pyproject.toml".to_owned(),
        line: None,
        label: "pytest defaults".to_owned(),
    });

    let globs = pytest_test_globs(&testpaths, &python_files);
    let source_paths: Vec<String> = ctx
        .sources
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    for path in match_paths_against_globs(&source_paths, &globs) {
        contrib.entries.push(PluginEntry {
            spec: crate::config::EntrySpec { path, symbol: None },
            context: FileContext::Test,
            origin: origin.clone(),
        });
    }

    for file in &ctx.sources.files {
        if file.path.ends_with("/conftest.py") || file.path == "conftest.py" {
            contrib.entries.push(PluginEntry {
                spec: crate::config::EntrySpec {
                    path: file.path.clone(),
                    symbol: None,
                },
                context: FileContext::Test,
                origin: ReferenceOrigin {
                    file: file.path.clone(),
                    line: None,
                    label: "conftest.py".to_owned(),
                },
            });
        }
    }

    if let Some(table) = pyproject_table.as_ref()
        && let Some(options) = pytest_ini_options_from_pyproject(table)
        && let Some(plugins) = options.get("pytest_plugins")
    {
        let plugin_modules = collect_pytest_plugins(plugins);
        for module in plugin_modules {
            contrib.module_refs.push(ModuleReference {
                module,
                origin: origin.clone(),
            });
        }
    }

    contrib.binary_usages.push(BinaryUsage {
        binary: "pytest".to_owned(),
        origin,
    });

    if !has_explicit_config
        && contrib.entries.is_empty()
        && !super::util::manifest_has_dependency(ctx.manifest, "pytest")
    {
        contrib.binary_usages.clear();
    }

    if contrib.entries.is_empty() && contrib.binary_usages.is_empty() {
        warnings.push(PluginsWarning::PluginNoOp {
            plugin: PluginId::Pytest,
        });
    }

    (contrib, warnings)
}

/// Read a pytest option given either as a comma/newline-separated string or as
/// an array of strings.
fn str_list(options: &toml::Table, key: &str) -> Vec<String> {
    match options.get(key) {
        Some(toml::Value::String(value)) => parse_path_list(value),
        Some(toml::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn collect_pytest_plugins(value: &toml::Value) -> Vec<String> {
    match value {
        toml::Value::String(module) => vec![module.clone()],
        toml::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pytest_test_globs_use_defaults() {
        let globs = pytest_test_globs(&[], &[]);
        assert!(globs.contains(&"tests/**/test_*.py".to_owned()));
    }
}
