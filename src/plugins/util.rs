//! Shared helpers for plugin extractors.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use toml::Value;

use crate::manifest::ManifestError;
use crate::manifest::literals::LiteralScan;
use crate::manifest::util::{read_to_string, relative_path as manifest_relative_path};

use super::error::PluginsError;
use super::types::ReferenceOrigin;

/// INI section key-value pairs.
pub type IniSection = BTreeMap<String, String>;

/// Read a single INI section from a config file.
pub fn read_ini_section(path: &Path, section_name: &str) -> Result<IniSection, PluginsError> {
    let contents = read_to_string(path).map_err(manifest_io_error)?;
    Ok(parse_ini_section(&contents, section_name))
}

fn manifest_io_error(error: ManifestError) -> PluginsError {
    match error {
        ManifestError::Io { path, source } => PluginsError::Io { path, source },
        other => PluginsError::InvalidConfig {
            path: other.to_string(),
            detail: other.to_string(),
        },
    }
}

/// Read `pyproject.toml` as a TOML table.
pub fn read_pyproject_table(path: &Path) -> Result<toml::Table, PluginsError> {
    let contents = read_to_string(path).map_err(manifest_io_error)?;
    toml::from_str(&contents).map_err(|source| PluginsError::InvalidConfig {
        path: relative_path(path.parent().unwrap_or(path), path),
        detail: source.to_string(),
    })
}

/// Parse one INI section from file contents.
pub fn parse_ini_section(contents: &str, section_name: &str) -> IniSection {
    let mut in_section = false;
    let mut section = IniSection::new();

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = trimmed[1..trimmed.len() - 1].trim();
            in_section = name == section_name;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, value)) = split_ini_assignment(trimmed) {
            section.insert(key.to_owned(), value.to_owned());
        }
    }

    section
}

fn split_ini_assignment(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(['=', ':'])?;
    let key = key.trim();
    let value = rest.trim();
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Return the `[tool.pytest.ini_options]` table when present.
pub fn pytest_ini_options_from_pyproject(table: &toml::Table) -> Option<&toml::Table> {
    table
        .get("tool")
        .and_then(Value::as_table)
        .and_then(|tool| tool.get("pytest"))
        .and_then(Value::as_table)
        .and_then(|pytest| pytest.get("ini_options"))
        .and_then(Value::as_table)
}

/// Return the `[tool.uvicorn]` table when present.
pub fn uvicorn_tool_from_pyproject(table: &toml::Table) -> Option<&toml::Table> {
    table
        .get("tool")
        .and_then(Value::as_table)
        .and_then(|tool| tool.get("uvicorn"))
        .and_then(Value::as_table)
}

/// Parse a comma- or newline-separated pytest path list.
pub fn parse_path_list(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Build test file glob patterns from pytest options.
pub fn pytest_test_globs(testpaths: &[String], python_files: &[String]) -> Vec<String> {
    let file_patterns = if python_files.is_empty() {
        vec!["test_*.py".to_owned(), "*_test.py".to_owned()]
    } else {
        python_files.to_vec()
    };

    let roots = if testpaths.is_empty() {
        vec!["tests".to_owned()]
    } else {
        testpaths.to_vec()
    };

    let mut globs = Vec::new();
    for root in roots {
        let normalized = root.trim_end_matches('/');
        for pattern in &file_patterns {
            globs.push(format!("{normalized}/**/{pattern}"));
        }
    }
    globs
}

/// Match discovered file paths against glob patterns.
pub fn match_paths_against_globs(paths: &[String], patterns: &[String]) -> Vec<String> {
    let Ok(glob_matcher) = build_glob_set(patterns) else {
        return Vec::new();
    };
    let mut hits: Vec<String> = paths
        .iter()
        .filter(|path| glob_matcher.is_match(path))
        .cloned()
        .collect();
    hits.sort();
    hits.dedup();
    hits
}

fn build_glob_set(patterns: &[String]) -> Result<GlobSet, globset::Error> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }
    builder.build()
}

/// Convert a dotted module path to a root-relative `.py` file path.
pub fn module_to_py_path(module: &str) -> String {
    format!("{}.py", module.replace('.', "/"))
}

/// Extract `DJANGO_SETTINGS_MODULE` from `manage.py`.
pub fn extract_django_settings_module(contents: &str) -> Option<String> {
    django_settings_re()
        .captures(contents)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_owned())
}

/// Parse `module:symbol` from a uvicorn-style target string.
pub fn parse_module_symbol(value: &str) -> Option<(String, String)> {
    let trimmed = value.trim();
    let (module, symbol) = trimmed.split_once(':')?;
    if module.is_empty() || symbol.is_empty() {
        return None;
    }
    Some((module.to_owned(), symbol.to_owned()))
}

/// Parse `uvicorn pkg.module:app` from a script target string.
pub fn parse_uvicorn_script_target(value: &str) -> Option<(String, String)> {
    uvicorn_script_re().captures(value).and_then(|caps| {
        let module = caps.get(1)?.as_str().to_owned();
        let symbol = caps.get(2)?.as_str().to_owned();
        Some((module, symbol))
    })
}

#[allow(clippy::expect_used)]
fn django_settings_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"setdefault\s*\(\s*["']DJANGO_SETTINGS_MODULE["']\s*,\s*["']([^"']+)["']"#)
            .expect("valid django settings regex")
    })
}

#[allow(clippy::expect_used)]
fn uvicorn_script_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"uvicorn\s+([A-Za-z_][A-Za-z0-9_.]*):([A-Za-z_][A-Za-z0-9_]*)")
            .expect("valid uvicorn script regex")
    })
}

/// Check whether a distribution name appears in manifest dependencies.
pub fn manifest_has_dependency(manifest: &crate::manifest::LoadedManifest, name: &str) -> bool {
    let needle = name.to_ascii_lowercase();
    manifest.dependencies.iter().any(|dep| dep.name == needle)
}

/// Root-relative path using `/` separators.
pub fn relative_path(root: &Path, path: &Path) -> String {
    manifest_relative_path(root, path)
}

/// Build a reference origin for a config file.
pub fn origin_for_file(root: &Path, path: &Path, label: impl Into<String>) -> ReferenceOrigin {
    ReferenceOrigin {
        file: relative_path(root, path),
        line: None,
        label: label.into(),
    }
}

/// Collect partial-parse field names from list literal scans.
pub fn partial_fields(scans: &BTreeMap<String, LiteralScan>) -> Vec<String> {
    scans
        .iter()
        .filter(|(_, scan)| !scan.complete)
        .map(|(field, _)| field.clone())
        .collect()
}

/// Find `settings.py` candidates under the project root (depth ≤ 4).
pub fn find_settings_candidates(root: &Path) -> Vec<String> {
    let mut candidates = Vec::new();
    collect_settings_files(root, root, 0, &mut candidates);
    candidates.sort();
    candidates
}

fn collect_settings_files(root: &Path, current: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if path.is_dir() {
            if file_name == ".git"
                || file_name == ".venv"
                || file_name == "node_modules"
                || file_name == "__pycache__"
            {
                continue;
            }
            collect_settings_files(root, &path, depth + 1, out);
        } else if file_name == "settings.py" {
            out.push(relative_path(root, &path));
        }
    }
}

/// Choose the best `settings.py` candidate when multiple exist.
pub fn choose_settings_path(
    candidates: &[String],
    preferred_module: Option<&str>,
    project_name: Option<&str>,
) -> (Option<String>, bool) {
    if candidates.is_empty() {
        return (None, false);
    }
    if candidates.len() == 1 {
        return (Some(candidates[0].clone()), false);
    }

    if let Some(module) = preferred_module {
        let preferred = module_to_py_path(module);
        if let Some(found) = candidates.iter().find(|path| **path == preferred) {
            return (Some((*found).clone()), true);
        }
    }

    if let Some(name) = project_name {
        let suffix = format!("/{name}/settings.py");
        if let Some(found) = candidates.iter().find(|path| path.ends_with(&suffix)) {
            return (Some((*found).clone()), true);
        }
    }

    (Some(candidates[0].clone()), true)
}

/// Resolve a file path under the project root.
pub fn root_join(root: &Path, rel: &str) -> PathBuf {
    root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ini_section_reads_pytest() {
        let contents = "[pytest]\ntestpaths = integration\npython_files = test_*.py\n";
        let section = parse_ini_section(contents, "pytest");
        assert_eq!(
            section.get("testpaths").map(String::as_str),
            Some("integration")
        );
    }

    #[test]
    fn module_to_py_path_converts_dots() {
        assert_eq!(
            module_to_py_path("myproject.settings"),
            "myproject/settings.py"
        );
    }

    #[test]
    fn parse_uvicorn_script_target_extracts_symbol() {
        let parsed = parse_uvicorn_script_target("uvicorn pkg.main:app").expect("parsed");
        assert_eq!(parsed, ("pkg.main".to_owned(), "app".to_owned()));
    }
}
