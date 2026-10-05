//! pytest plugin extractor.

#![allow(clippy::too_many_lines)]

use std::path::Path;

use crate::config::PluginId;
use crate::path_util::rel_to_root;
use crate::sources::FileContext;

use super::context::PluginContext;
use super::types::{
    BinaryUsage, ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin,
};
use super::util::{
    IniSection, match_paths_against_globs, origin_for_file, parse_path_list,
    pytest_ini_options_from_pyproject, pytest_test_globs, read_ini_section, read_pyproject_table,
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
                        path: rel_to_root(root, &path),
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

    (contrib, warnings)
}

/// The `sys.path` settings pytest applies while importing tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PytestImportSettings {
    /// `pythonpath` entries, relative to the project root.
    pub pythonpath: Vec<String>,
    /// `--import-mode=importlib` in `addopts`: test and conftest directories
    /// are not put on `sys.path`.
    pub importlib: bool,
}

/// Read [`PytestImportSettings`] from the config file pytest itself picks.
///
/// `pytest.ini` / `.pytest.ini` win even without a `[pytest]` section, then
/// the first of `pyproject.toml`, `tox.ini` and `setup.cfg` with a pytest
/// section.
#[must_use]
pub fn import_settings(root: &Path) -> PytestImportSettings {
    for file in ["pytest.ini", ".pytest.ini"] {
        let path = root.join(file);
        if path.is_file() {
            return read_ini_section(&path, "pytest")
                .map(|section| ini_import_settings(&section))
                .unwrap_or_default();
        }
    }
    if let Some(settings) = pyproject_import_settings(&root.join("pyproject.toml")) {
        return settings;
    }
    for (file, section_name) in [("tox.ini", "pytest"), ("setup.cfg", "tool:pytest")] {
        let path = root.join(file);
        if !path.is_file() {
            continue;
        }
        if let Ok(section) = read_ini_section(&path, section_name)
            && !section.is_empty()
        {
            return ini_import_settings(&section);
        }
    }
    PytestImportSettings::default()
}

fn pyproject_import_settings(path: &Path) -> Option<PytestImportSettings> {
    if !path.is_file() {
        return None;
    }
    let table = read_pyproject_table(path).ok()?;
    let options = pytest_ini_options_from_pyproject(&table)?;
    let addopts = options
        .get("addopts")
        .map_or_else(Vec::new, |value| match value {
            toml::Value::String(text) => split_words(text),
            toml::Value::Array(items) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        });
    let pythonpath = match options.get("pythonpath") {
        Some(toml::Value::String(text)) => split_words(text),
        _ => str_list(options, "pythonpath"),
    };
    Some(PytestImportSettings {
        pythonpath,
        importlib: has_importlib_mode(&addopts),
    })
}

fn ini_import_settings(section: &IniSection) -> PytestImportSettings {
    let words = |key: &str| section.get(key).map_or_else(Vec::new, |v| split_words(v));
    PytestImportSettings {
        pythonpath: words("pythonpath"),
        importlib: has_importlib_mode(&words("addopts")),
    }
}

fn split_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn has_importlib_mode(addopts: &[String]) -> bool {
    addopts.iter().enumerate().any(|(index, word)| {
        word == "--import-mode=importlib"
            || (word == "--import-mode"
                && addopts
                    .get(index + 1)
                    .is_some_and(|next| next == "importlib"))
    })
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
        // pytest's defaults: `testpaths` unset → rootdir, `python_files` → `test_*.py *_test.py`.
        assert_eq!(
            globs,
            vec!["**/test_*.py".to_owned(), "**/*_test.py".to_owned()]
        );
    }

    #[test]
    fn pytest_test_globs_treat_dot_testpath_as_rootdir() {
        let globs = pytest_test_globs(
            &[".".to_owned(), "./test/".to_owned()],
            &["check_*.py".to_owned()],
        );
        assert_eq!(
            globs,
            vec!["**/check_*.py".to_owned(), "test/**/check_*.py".to_owned()]
        );
    }

    #[test]
    fn import_settings_follow_pytest_config_precedence() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path();
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.pytest.ini_options]\npythonpath = [\"src\"]\n",
        )
        .expect("write pyproject");
        assert_eq!(import_settings(root).pythonpath, ["src"]);

        std::fs::write(
            root.join("pytest.ini"),
            "[pytest]\naddopts = -q --import-mode importlib\n",
        )
        .expect("write pytest.ini");
        assert_eq!(
            import_settings(root),
            PytestImportSettings {
                pythonpath: Vec::new(),
                importlib: true,
            }
        );
    }
}
