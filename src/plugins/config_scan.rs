//! Static config scanning for CLI / dev-tool usage (Phase 1.5 §4.A).

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use toml::Value;

use crate::manifest::{DependencyContext, normalize_distribution_name};
use crate::resolver::build_binary_map;

use super::context::PluginContext;
use super::types::{BinaryUsage, ReferenceOrigin};
use super::util::{read_pyproject_table, relative_path};

/// Output from config scanning.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConfigScanResult {
    /// CLI binaries referenced from configuration.
    pub binary_usages: Vec<BinaryUsage>,
    /// Distributions used without a distinct CLI name (themes, tox extras, etc.).
    pub used_distributions: Vec<String>,
}

/// Scan project configuration for dev-tool / CLI usage.
#[must_use]
pub fn scan_config(ctx: &PluginContext<'_>) -> ConfigScanResult {
    let root = ctx.root.path.as_path();
    let mut result = ConfigScanResult::default();
    let mut seen_binaries: HashSet<(String, String)> = HashSet::new();

    scan_pyproject_tools(root, &mut result, &mut seen_binaries);
    scan_manifest_entry_points(ctx, &mut result, &mut seen_binaries);
    scan_mkdocs_config(root, &mut result, &mut seen_binaries);
    scan_pre_commit_config(root, &mut result, &mut seen_binaries);
    scan_tox_config(root, ctx, &mut result, &mut seen_binaries);
    scan_shell_scripts(root, &mut result, &mut seen_binaries);

    result.used_distributions.sort();
    result.used_distributions.dedup();
    result
}

fn scan_pyproject_tools(
    root: &Path,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
) {
    let path = root.join("pyproject.toml");
    if !path.is_file() {
        return;
    }
    let Ok(table) = read_pyproject_table(&path) else {
        return;
    };
    let rel = relative_path(root, &path);
    let Some(tool) = table.get("tool").and_then(Value::as_table) else {
        return;
    };
    for key in tool.keys() {
        if let Some(binary) = tool_key_to_binary(key) {
            push_binary(
                result,
                seen,
                binary,
                ReferenceOrigin {
                    file: rel.clone(),
                    line: None,
                    label: format!("tool.{key}"),
                },
            );
        }
    }
}

fn scan_manifest_entry_points(
    ctx: &PluginContext<'_>,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
) {
    for entry in &ctx.manifest.entry_points {
        if entry.group != "console_scripts" && entry.group != "gui_scripts" {
            continue;
        }
        if is_known_binary(&entry.name) {
            push_binary(
                result,
                seen,
                &entry.name,
                ReferenceOrigin {
                    file: entry.origin.file.clone(),
                    line: entry.origin.line,
                    label: format!("entry_points.{}", entry.name),
                },
            );
        }
    }
}

fn scan_mkdocs_config(
    root: &Path,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
) {
    for name in ["mkdocs.yml", "mkdocs.yaml"] {
        let path = root.join(name);
        if !path.is_file() {
            continue;
        }
        let rel = relative_path(root, &path);
        push_binary(
            result,
            seen,
            "mkdocs",
            ReferenceOrigin {
                file: rel,
                line: None,
                label: name.to_owned(),
            },
        );
        if let Ok(contents) = std::fs::read_to_string(&path) {
            for distribution in mkdocs_used_distributions(&contents) {
                push_distribution(result, distribution);
            }
        }
        break;
    }
}

fn mkdocs_used_distributions(contents: &str) -> Vec<&'static str> {
    let mut distributions = Vec::new();
    if mkdocs_theme_name(contents).is_some_and(|theme| theme == "material")
        || contents.contains("mkdocs-material")
    {
        distributions.push("mkdocs-material");
    }

    for plugin in mkdocs_plugin_names(contents) {
        if let Some(distribution) = mkdocs_plugin_distribution(&plugin) {
            distributions.push(distribution);
        }
    }

    distributions.sort_unstable();
    distributions.dedup();
    distributions
}

fn mkdocs_theme_name(contents: &str) -> Option<String> {
    let mut in_theme = false;
    let mut theme_indent = 0usize;

    for line in contents.lines() {
        let trimmed = strip_yaml_comment(line).trim();
        if trimmed.is_empty() {
            continue;
        }
        let indent = leading_spaces(line);
        if trimmed == "theme:" {
            in_theme = true;
            theme_indent = indent;
            continue;
        }
        if in_theme && indent <= theme_indent {
            in_theme = false;
        }
        if let Some(value) = trimmed.strip_prefix("theme:") {
            return Some(unquote_yaml_scalar(value.trim()).to_owned());
        }
        if in_theme && let Some(value) = trimmed.strip_prefix("name:") {
            return Some(unquote_yaml_scalar(value.trim()).to_owned());
        }
    }

    None
}

fn mkdocs_plugin_names(contents: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_plugins = false;
    let mut plugins_indent = 0usize;

    for line in contents.lines() {
        let trimmed = strip_yaml_comment(line).trim();
        if trimmed.is_empty() {
            continue;
        }
        let indent = leading_spaces(line);
        if trimmed == "plugins:" {
            in_plugins = true;
            plugins_indent = indent;
            continue;
        }
        if in_plugins && indent <= plugins_indent {
            in_plugins = false;
        }
        if !in_plugins {
            continue;
        }
        let Some(item) = trimmed.strip_prefix('-') else {
            continue;
        };
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let name = item
            .split_once(':')
            .map_or(item, |(name, _)| name)
            .trim();
        if !name.is_empty() {
            names.push(unquote_yaml_scalar(name).to_owned());
        }
    }

    names
}

fn mkdocs_plugin_distribution(plugin: &str) -> Option<&'static str> {
    match plugin {
        "mkdocstrings" => Some("mkdocstrings"),
        "autorefs" => Some("mkdocs-autorefs"),
        "include-markdown" => Some("mkdocs-include-markdown-plugin"),
        "redirects" => Some("mkdocs-redirects"),
        "minify" => Some("mkdocs-minify-plugin"),
        _ => None,
    }
}

fn strip_yaml_comment(line: &str) -> &str {
    line.split_once('#').map_or(line, |(before, _)| before)
}

fn unquote_yaml_scalar(value: &str) -> &str {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
}

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

fn scan_pre_commit_config(
    root: &Path,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
) {
    let path = root.join(".pre-commit-config.yaml");
    if !path.is_file() {
        return;
    }
    let rel = relative_path(root, &path);
    let origin = ReferenceOrigin {
        file: rel,
        line: None,
        label: ".pre-commit-config.yaml".to_owned(),
    };
    push_binary(result, seen, "pre-commit", origin.clone());

    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    for line in contents.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed
            .strip_prefix("- id:")
            .or_else(|| trimmed.strip_prefix("id:"))
        else {
            continue;
        };
        let hook_id = rest.trim().trim_matches(['"', '\'']);
        if let Some(binary) = hook_id_to_binary(hook_id) {
            push_binary(result, seen, binary, origin.clone());
        }
    }
}

fn scan_tox_config(
    root: &Path,
    ctx: &PluginContext<'_>,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
) {
    let path = root.join("tox.ini");
    if !path.is_file() {
        return;
    }
    let rel = relative_path(root, &path);
    let origin = ReferenceOrigin {
        file: rel,
        line: None,
        label: "tox.ini".to_owned(),
    };
    push_binary(result, seen, "tox", origin.clone());

    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    scan_tox_contents(ctx, &contents, result);
    scan_lines_for_binaries(&contents, result, seen, &origin);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToxListKey {
    Deps,
    Extras,
}

fn scan_tox_contents(ctx: &PluginContext<'_>, contents: &str, result: &mut ConfigScanResult) {
    let mut current_extras: Vec<String> = Vec::new();
    let mut current_key: Option<ToxListKey> = None;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            current_extras.clear();
            current_key = None;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if let Some(key) = current_key
            && line.chars().next().is_some_and(char::is_whitespace)
        {
            match key {
                ToxListKey::Deps => push_tox_dependency(trimmed, result),
                ToxListKey::Extras => {
                    current_extras.extend(push_tox_extras(ctx, trimmed, result));
                }
            }
            continue;
        }
        current_key = None;
        if let Some(value) = trimmed.strip_prefix("extras =") {
            current_key = Some(ToxListKey::Extras);
            current_extras = push_tox_extras(ctx, value, result);
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("extras +=") {
            current_key = Some(ToxListKey::Extras);
            current_extras.extend(push_tox_extras(ctx, value, result));
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("deps =") {
            current_key = Some(ToxListKey::Deps);
            if value.trim().is_empty() {
                for extra in &current_extras {
                    mark_optional_extra_dependencies(ctx, extra, result);
                }
            } else {
                push_tox_dependency(value, result);
            }
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("deps +=") {
            current_key = Some(ToxListKey::Deps);
            push_tox_dependency(value, result);
        }
    }
}

fn push_tox_extras(
    ctx: &PluginContext<'_>,
    raw: &str,
    result: &mut ConfigScanResult,
) -> Vec<String> {
    let extras = raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for extra in &extras {
        mark_optional_extra_dependencies(ctx, extra, result);
    }
    extras
}

fn push_tox_dependency(raw: &str, result: &mut ConfigScanResult) {
    if let Some(name) = extract_requirement_name(raw) {
        push_distribution(result, &name);
    }
}

fn extract_requirement_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let end = trimmed
        .find(['[', ';', '<', '>', '=', '!', ' '])
        .unwrap_or(trimmed.len());
    let name = trimmed.get(..end)?.trim();
    if name.is_empty() {
        None
    } else {
        Some(normalize_distribution_name(name))
    }
}

fn mark_optional_extra_dependencies(
    ctx: &PluginContext<'_>,
    extra: &str,
    result: &mut ConfigScanResult,
) {
    for dep in &ctx.manifest.dependencies {
        if matches!(
            dep.context,
            DependencyContext::OptionalExtra(ref name) if name == extra
        ) {
            push_distribution(result, &dep.name);
        }
    }
}

fn scan_shell_scripts(
    root: &Path,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
) {
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
            let rel = relative_path(root, &path);
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let origin = ReferenceOrigin {
                file: rel,
                line: None,
                label: path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("script")
                    .to_owned(),
            };
            scan_lines_for_binaries(&contents, result, seen, &origin);
        }
    }
}

fn scan_lines_for_binaries(
    contents: &str,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    origin: &ReferenceOrigin,
) {
    for (line_index, line) in contents.lines().enumerate() {
        let mut line_origin = origin.clone();
        line_origin.line = u32::try_from(line_index + 1).ok();
        for token in line.split_whitespace() {
            let token =
                token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '.');
            let token = token
                .strip_prefix("${PREFIX}")
                .or_else(|| token.strip_prefix("${prefix}"))
                .unwrap_or(token);
            let token = token.strip_prefix("venv/bin/").unwrap_or(token);
            if is_known_binary(token) {
                push_binary(result, seen, token, line_origin.clone());
            }
            if token == "sphinx-build" {
                push_binary(result, seen, "sphinx-build", line_origin.clone());
                push_distribution(result, "sphinx");
            }
        }
    }
}

fn push_binary(
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    binary: &str,
    origin: ReferenceOrigin,
) {
    let key = (
        binary.to_owned(),
        format!("{}:{}", origin.file, origin.line.unwrap_or_default()),
    );
    if !seen.insert(key) {
        return;
    }
    result.binary_usages.push(BinaryUsage {
        binary: binary.to_owned(),
        origin,
    });
}

fn push_distribution(result: &mut ConfigScanResult, distribution: &str) {
    result
        .used_distributions
        .push(normalize_distribution_name(distribution));
}

fn tool_key_to_binary(key: &str) -> Option<&'static str> {
    match key {
        "mypy" => Some("mypy"),
        "ruff" => Some("ruff"),
        "black" => Some("black"),
        "isort" => Some("isort"),
        "pylint" => Some("pylint"),
        "bandit" => Some("bandit"),
        "coverage" => Some("coverage"),
        "pytest" => Some("pytest"),
        "tox" => Some("tox"),
        "nox" => Some("nox"),
        "twine" => Some("twine"),
        "towncrier" => Some("towncrier"),
        "cogapp" => Some("cogapp"),
        "build" => Some("build"),
        "mkdocs" => Some("mkdocs"),
        "pre-commit" | "pre_commit" => Some("pre-commit"),
        _ => None,
    }
}

fn hook_id_to_binary(hook_id: &str) -> Option<&'static str> {
    match hook_id {
        "black" => Some("black"),
        "ruff" | "ruff-format" => Some("ruff"),
        "mypy" => Some("mypy"),
        "isort" => Some("isort"),
        "pytest" => Some("pytest"),
        "bandit" => Some("bandit"),
        "flake8" => Some("flake8"),
        "pyupgrade" => Some("pyupgrade"),
        "autopep8" => Some("autopep8"),
        _ => None,
    }
}

fn is_known_binary(name: &str) -> bool {
    if tool_key_to_binary(name).is_some() || hook_id_to_binary(name).is_some() {
        return true;
    }
    build_binary_map(
        &crate::default_config(),
        &crate::resolver::VenvIndex::default(),
    )
    .contains_key(name)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::manifest::{
        DependencyOrigin, LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata,
    };
    use crate::sources::{DiscoveredSources, LayoutInfo, ProjectLayout};

    fn empty_manifest(root: ProjectRoot) -> LoadedManifest {
        LoadedManifest {
            root,
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn detects_tool_tables_in_pyproject() {
        let dir = std::env::temp_dir().join("chokkin-config-scan-tools");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(
            dir.join("pyproject.toml"),
            "[tool.mypy]\nstrict = true\n\n[tool.ruff]\nline-length = 88\n",
        )
        .expect("write pyproject");

        let root = ProjectRoot {
            path: dir.clone(),
            marker: RootMarker::PyProjectToml,
            start: dir,
        };
        let config = crate::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Unknown,
                packages: Vec::new(),
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let manifest = empty_manifest(root.clone());
        let ctx = PluginContext {
            root: &root,
            config: &config,
            sources: &sources,
            manifest: &manifest,
        };
        let result = scan_config(&ctx);
        let binaries: BTreeSet<_> = result
            .binary_usages
            .iter()
            .map(|usage| usage.binary.as_str())
            .collect();
        assert!(binaries.contains("mypy"));
        assert!(binaries.contains("ruff"));
    }

    #[test]
    fn detects_mkdocs_material_theme() {
        let dir = std::env::temp_dir().join("chokkin-config-scan-mkdocs");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(
            dir.join("mkdocs.yml"),
            "site_name: demo\ntheme:\n  name: material\nplugins:\n  - search\n  - mkdocstrings:\n      handlers:\n        python: {}\n  - autorefs\n",
        )
        .expect("write mkdocs");

        let root = ProjectRoot {
            path: dir.clone(),
            marker: RootMarker::PyProjectToml,
            start: dir,
        };
        let config = crate::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Unknown,
                packages: Vec::new(),
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let manifest = empty_manifest(root.clone());
        let ctx = PluginContext {
            root: &root,
            config: &config,
            sources: &sources,
            manifest: &manifest,
        };
        let result = scan_config(&ctx);
        assert!(
            result
                .binary_usages
                .iter()
                .any(|usage| usage.binary == "mkdocs")
        );
        assert!(
            result
                .used_distributions
                .contains(&"mkdocs-material".to_owned())
        );
        assert!(
            result
                .used_distributions
                .contains(&"mkdocstrings".to_owned())
        );
        assert!(
            result
                .used_distributions
                .contains(&"mkdocs-autorefs".to_owned())
        );
    }

    #[test]
    fn tox_extras_mark_optional_dependencies_used() {
        let dir = std::env::temp_dir().join("chokkin-config-scan-tox");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(
            dir.join("tox.ini"),
            "[testenv:docs]\nextras = docs\ncommands = sphinx-build -b html docs docs/_build\n",
        )
        .expect("write tox");

        let root = ProjectRoot {
            path: dir.clone(),
            marker: RootMarker::PyProjectToml,
            start: dir,
        };
        let mut manifest = empty_manifest(root.clone());
        manifest
            .dependencies
            .push(crate::manifest::DeclaredDependency {
                name: "sphinx".to_owned(),
                extras: Vec::new(),
                marker: None,
                specifier: None,
                context: DependencyContext::OptionalExtra("docs".to_owned()),
                origin: DependencyOrigin {
                    file: "pyproject.toml".to_owned(),
                    line: Some(1),
                    label: "project.optional-dependencies.docs[0]".to_owned(),
                },
                opaque: false,
            });
        let config = crate::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Unknown,
                packages: Vec::new(),
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let ctx = PluginContext {
            root: &root,
            config: &config,
            sources: &sources,
            manifest: &manifest,
        };
        let result = scan_config(&ctx);
        assert!(result.used_distributions.contains(&"sphinx".to_owned()));
        assert!(
            result
                .binary_usages
                .iter()
                .any(|usage| usage.binary == "tox")
        );
        assert!(result.binary_usages.iter().any(|usage| {
            usage.binary == "sphinx-build"
                && usage.origin.file == "tox.ini"
                && usage.origin.line == Some(3)
        }));
    }

    #[test]
    fn tox_multiline_deps_and_extras_mark_distributions_used() {
        let dir = std::env::temp_dir().join("chokkin-config-scan-tox-multiline");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(
            dir.join("tox.ini"),
            "[testenv:docs]\nextras =\n    docs\ndeps =\n    pytest\n    requests>=2\n",
        )
        .expect("write tox");

        let root = ProjectRoot {
            path: dir.clone(),
            marker: RootMarker::PyProjectToml,
            start: dir,
        };
        let mut manifest = empty_manifest(root.clone());
        manifest
            .dependencies
            .push(crate::manifest::DeclaredDependency {
                name: "sphinx".to_owned(),
                extras: Vec::new(),
                marker: None,
                specifier: None,
                context: DependencyContext::OptionalExtra("docs".to_owned()),
                origin: DependencyOrigin {
                    file: "pyproject.toml".to_owned(),
                    line: Some(1),
                    label: "project.optional-dependencies.docs[0]".to_owned(),
                },
                opaque: false,
            });
        let config = crate::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Unknown,
                packages: Vec::new(),
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let ctx = PluginContext {
            root: &root,
            config: &config,
            sources: &sources,
            manifest: &manifest,
        };
        let result = scan_config(&ctx);
        assert!(result.used_distributions.contains(&"sphinx".to_owned()));
        assert!(result.used_distributions.contains(&"pytest".to_owned()));
        assert!(result.used_distributions.contains(&"requests".to_owned()));
    }
}
