//! Static config scanning for CLI / dev-tool usage (Phase 1.5 §4.A).

use std::collections::HashSet;
use std::path::Path;

use toml::Value;

use crate::manifest::{DependencyContext, normalize_distribution_name};
use crate::resolver::build_binary_map;

use super::context::PluginContext;
use super::types::{BinaryUsage, ReferenceOrigin};
use super::util::{read_pyproject_table, relative_path};

/// Output from config scanning.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
        if let Ok(contents) = std::fs::read_to_string(&path)
            && mkdocs_uses_material_theme(&contents)
        {
            push_distribution(result, "mkdocs-material");
        }
        break;
    }
}

fn mkdocs_uses_material_theme(contents: &str) -> bool {
    contents.contains("name: material")
        || contents.contains("name: 'material'")
        || contents.contains("name: \"material\"")
        || contents.contains("mkdocs-material")
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

fn scan_tox_contents(ctx: &PluginContext<'_>, contents: &str, result: &mut ConfigScanResult) {
    let mut current_extras: Vec<String> = Vec::new();

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            current_extras.clear();
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("extras =") {
            current_extras = value
                .split([',', '\n'])
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect();
            for extra in &current_extras {
                mark_optional_extra_dependencies(ctx, extra, result);
            }
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("deps =") {
            push_tox_dependency(value, result);
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("deps +=") {
            push_tox_dependency(value, result);
        }
    }
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
    for line in contents.lines() {
        for token in line.split_whitespace() {
            let token =
                token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '.');
            let token = token
                .strip_prefix("${PREFIX}")
                .or_else(|| token.strip_prefix("${prefix}"))
                .unwrap_or(token);
            let token = token.strip_prefix("venv/bin/").unwrap_or(token);
            if is_known_binary(token) {
                push_binary(result, seen, token, origin.clone());
            }
            if token == "sphinx-build" {
                push_binary(result, seen, "sphinx-build", origin.clone());
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
    let key = (binary.to_owned(), origin.file.clone());
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
        let dir = std::env::temp_dir().join("yokei-config-scan-tools");
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
        let dir = std::env::temp_dir().join("yokei-config-scan-mkdocs");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(
            dir.join("mkdocs.yml"),
            "site_name: demo\ntheme:\n  name: material\n",
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
    }

    #[test]
    fn tox_extras_mark_optional_dependencies_used() {
        let dir = std::env::temp_dir().join("yokei-config-scan-tox");
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
    }
}
