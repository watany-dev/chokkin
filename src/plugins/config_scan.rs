//! Static config scanning for CLI / dev-tool usage (Phase 1.5 §4.A).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::manifest::{DependencyContext, normalize_distribution_name};
use crate::path_util::rel_to_root;
use crate::resolver::{VenvIndex, build_binary_map};

use super::commands::{KnownBinary, SourceHits, command_binaries};
use super::config_text::{PyprojectDoc, leading_spaces, logical_lines, origin_at};
use super::context::PluginContext;
use super::types::{BinaryUsage, ModuleReference, ReferenceOrigin};
use super::{task_files, tool_plugins};

/// Output from config scanning.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConfigScanResult {
    /// CLI binaries referenced from configuration.
    pub binary_usages: Vec<BinaryUsage>,
    /// Distributions used without a distinct CLI name (themes, tox extras, etc.).
    pub used_distributions: Vec<String>,
    /// Modules named as plugins or callables in config (pytest `-p`, mypy
    /// `plugins`, PDM `call`).
    #[serde(default)]
    pub module_refs: Vec<ModuleReference>,
}

const MKDOCS_CONFIG_NAMES: [&str; 2] = ["mkdocs.yml", "mkdocs.yaml"];
const PRE_COMMIT_CONFIG: &str = ".pre-commit-config.yaml";
const TOX_CONFIG: &str = "tox.ini";
const SCRIPT_DIRS: [&str; 2] = ["scripts", "bin"];

/// Files [`scan_config`] may read beyond the config and manifest inputs.
///
/// Every existing candidate is listed, not only the ones a scan read, so a
/// cache entry is invalidated when a candidate appears after it was written.
#[must_use]
pub fn scan_input_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = MKDOCS_CONFIG_NAMES
        .into_iter()
        .chain([PRE_COMMIT_CONFIG, TOX_CONFIG])
        .map(|name| root.join(name))
        .filter(|path| path.is_file())
        .collect();
    paths.extend(shell_script_paths(root));
    paths.extend(task_files::input_paths(root));
    paths.extend(tool_plugins::input_paths(root));
    paths
}

/// Scan project configuration for dev-tool / CLI usage.
#[must_use]
pub fn scan_config(ctx: &PluginContext<'_>) -> ConfigScanResult {
    let root = ctx.root.path.as_path();
    let mut result = ConfigScanResult::default();
    let mut seen_binaries: HashSet<(String, String)> = HashSet::new();
    let pyproject = PyprojectDoc::load(root);
    let binary_map = build_binary_map(ctx.config, &VenvIndex::default());
    let known = |name: &str| {
        binary_map.contains_key(name)
            || tool_key_to_binary(name).is_some()
            || hook_id_to_binary(name).is_some()
    };

    scan_pyproject_tools(pyproject.as_ref(), &mut result, &mut seen_binaries);
    scan_manifest_entry_points(ctx, &mut result, &mut seen_binaries, &known);
    scan_mkdocs_config(root, &mut result, &mut seen_binaries);
    scan_pre_commit_config(root, &mut result, &mut seen_binaries, &known);
    scan_tox_config(root, ctx, &mut result, &mut seen_binaries, &known);
    scan_shell_scripts(root, &mut result, &mut seen_binaries, &known);
    let hits = task_files::scan(root, pyproject.as_ref(), &known);
    merge_hits(&mut result, &mut seen_binaries, hits);
    let hits = tool_plugins::scan(root, pyproject.as_ref());
    merge_hits(&mut result, &mut seen_binaries, hits);

    result.used_distributions.sort();
    result.used_distributions.dedup();
    result
}

fn scan_pyproject_tools(
    pyproject: Option<&PyprojectDoc>,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
) {
    let Some(doc) = pyproject else {
        return;
    };
    let Some(tool) = doc.table("tool") else {
        return;
    };
    for key in tool.keys() {
        if let Some(binary) = tool_key_to_binary(key) {
            push_binary(result, seen, binary, doc.origin("tool", key));
        }
    }
}

fn merge_hits(
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    hits: SourceHits,
) {
    for usage in hits.binaries {
        push_binary(result, seen, &usage.binary, usage.origin);
    }
    for distribution in &hits.distributions {
        push_distribution(result, distribution);
    }
    result.module_refs.extend(hits.module_refs);
}

fn scan_manifest_entry_points(
    ctx: &PluginContext<'_>,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    known: KnownBinary<'_>,
) {
    for entry in &ctx.manifest.entry_points {
        if entry.group != "console_scripts" && entry.group != "gui_scripts" {
            continue;
        }
        if known(&entry.name) {
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
    for name in MKDOCS_CONFIG_NAMES {
        let path = root.join(name);
        if !path.is_file() {
            continue;
        }
        let rel = rel_to_root(root, &path);
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
        let name = item.split_once(':').map_or(item, |(name, _)| name).trim();
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
    value.trim().trim_matches('"').trim_matches('\'').trim()
}

fn scan_pre_commit_config(
    root: &Path,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    known: KnownBinary<'_>,
) {
    let path = root.join(PRE_COMMIT_CONFIG);
    if !path.is_file() {
        return;
    }
    let rel = rel_to_root(root, &path);
    let origin = ReferenceOrigin {
        file: rel.clone(),
        line: None,
        label: PRE_COMMIT_CONFIG.to_owned(),
    };
    push_binary(result, seen, "pre-commit", origin.clone());

    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    // Only `repo: local` hooks run a command from the project's own
    // environment; remote hooks install their tool into pre-commit's cache.
    let mut local_repo = false;
    for (index, line) in contents.lines().enumerate() {
        let trimmed = strip_yaml_comment(line).trim();
        let item = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim_start();
        if let Some(repo) = item.strip_prefix("repo:") {
            local_repo = unquote_yaml_scalar(repo) == "local";
            continue;
        }
        if let Some(hook_id) = item.strip_prefix("id:") {
            if let Some(binary) = hook_id_to_binary(unquote_yaml_scalar(hook_id)) {
                push_binary(result, seen, binary, origin.clone());
            }
            continue;
        }
        if local_repo && let Some(entry) = item.strip_prefix("entry:") {
            push_command(
                result,
                seen,
                unquote_yaml_scalar(entry),
                known,
                &origin_at(&rel, index, "pre-commit entry"),
            );
        }
    }
}

fn scan_tox_config(
    root: &Path,
    ctx: &PluginContext<'_>,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    known: KnownBinary<'_>,
) {
    let path = root.join(TOX_CONFIG);
    if !path.is_file() {
        return;
    }
    let rel = rel_to_root(root, &path);
    let origin = ReferenceOrigin {
        file: rel.clone(),
        line: None,
        label: TOX_CONFIG.to_owned(),
    };
    push_binary(result, seen, "tox", origin);

    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    scan_tox_contents(ctx, &contents, result);
    scan_tox_commands(&rel, &contents, result, seen, known);
}

const TOX_COMMAND_KEYS: [&str; 3] = ["commands", "commands_pre", "commands_post"];

/// Binaries run by `commands` / `commands_pre` / `commands_post` in any
/// section. Other keys (`deps`, `description`, `allowlist_externals`, ...)
/// name tools without running them, so their words are not commands (#494).
fn scan_tox_commands(
    rel: &str,
    contents: &str,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    known: KnownBinary<'_>,
) {
    let mut in_commands = false;
    for (index, line) in logical_lines(contents) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(['#', ';']) {
            continue;
        }
        let continuation = line.starts_with([' ', '\t']) && !trimmed.starts_with('[');
        let command = if continuation && in_commands {
            trimmed
        } else {
            let key_value = trimmed.split_once(['=', ':']);
            in_commands = key_value.is_some_and(|(key, _)| {
                TOX_COMMAND_KEYS.contains(&key.trim().trim_end_matches('+').trim())
            });
            if !in_commands {
                continue;
            }
            key_value.map_or("", |(_, value)| value.trim())
        };
        push_command(
            result,
            seen,
            &expand_tox_substitutions(strip_tox_factors(command)),
            known,
            &origin_at(rel, index, "tox commands"),
        );
    }
}

/// The command after a factor condition (`py38,!pypy: pytest` → `pytest`).
fn strip_tox_factors(command: &str) -> &str {
    match command.split_once(':') {
        Some((factors, rest))
            if !factors.is_empty()
                && factors.chars().all(|ch| {
                    ch.is_ascii_alphanumeric() || matches!(ch, ',' | '!' | '-' | '_' | '.')
                }) =>
        {
            rest.trim_start()
        },
        _ => command,
    }
}

/// `{envpython}` runs the env's interpreter and `{envbindir}/x` its scripts;
/// every other `{...}` substitution (`{posargs}`, `{toxinidir}`, `{env:X}`)
/// is dropped so it is never read as a command word.
fn expand_tox_substitutions(command: &str) -> String {
    let command = command
        .replace("{envpython}", "python")
        .replace("{env_python}", "python")
        .replace("{envbindir}/", "")
        .replace("{env_bin_dir}/", "");
    let mut depth = 0usize;
    let mut expanded = String::with_capacity(command.len());
    for ch in command.chars() {
        match ch {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => expanded.push(ch),
            _ => {},
        }
    }
    expanded
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
                },
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

/// Sorted so the first-seen origin of a binary does not depend on `read_dir` order.
fn shell_script_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for dir_name in SCRIPT_DIRS {
        let Ok(entries) = std::fs::read_dir(root.join(dir_name)) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        paths.extend(files);
    }
    paths
}

fn scan_shell_scripts(
    root: &Path,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    known: KnownBinary<'_>,
) {
    for path in shell_script_paths(root) {
        let rel = rel_to_root(root, &path);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !is_shell_script(&path, &contents) {
            continue;
        }
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("script")
            .to_owned();
        for (index, line) in logical_lines(&contents) {
            push_command(
                result,
                seen,
                strip_shell_comment(&line),
                known,
                &origin_at(&rel, index, &label),
            );
        }
    }
}

/// Python and other non-shell files under `scripts/` / `bin/` are read by the
/// parser or not at all; their words are not commands (#494).
fn is_shell_script(path: &Path, contents: &str) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("sh" | "bash" | "zsh" | "ksh") => true,
        Some(_) => false,
        None => shebang_interpreter(contents).is_none_or(|interpreter| interpreter.ends_with("sh")),
    }
}

/// Basename of the `#!` interpreter, past `env` and its flags
/// (`#!/usr/bin/env -S bash -e` → `bash`, `#!/bin/sh -eu` → `sh`).
fn shebang_interpreter(contents: &str) -> Option<&str> {
    let shebang = contents.lines().next()?.strip_prefix("#!")?;
    let mut words = shebang
        .split_whitespace()
        .map(|word| word.rsplit('/').next().unwrap_or(word));
    match words.next()? {
        "env" => words.find(|word| !word.starts_with('-')),
        interpreter => Some(interpreter),
    }
}

/// The line up to a `#` that starts a word (`$#` and `${#x}` are not comments).
fn strip_shell_comment(line: &str) -> &str {
    let mut end = line.len();
    for (index, _) in line.match_indices('#') {
        let previous = line.get(..index).and_then(|head| head.chars().last());
        if previous.is_none_or(char::is_whitespace) {
            end = index;
            break;
        }
    }
    line.get(..end).unwrap_or(line)
}

fn push_command(
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    command: &str,
    known: KnownBinary<'_>,
    origin: &ReferenceOrigin,
) {
    for binary in command_binaries(command, known) {
        if binary == "sphinx-build" {
            push_distribution(result, "sphinx");
        }
        push_binary(result, seen, &binary, origin.clone());
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::manifest::{
        DependencyOrigin, LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata,
    };
    use crate::parser::ParseSummary;
    use crate::sources::{DiscoveredSources, LayoutInfo, ProjectLayout};

    fn empty_manifest(root: ProjectRoot) -> LoadedManifest {
        LoadedManifest {
            root,
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings::default(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    /// Scan a throwaway project made of `files` (relative path, contents).
    fn scan_files(files: &[(&str, &str)]) -> ConfigScanResult {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().to_path_buf();
        for (name, contents) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create dir");
            }
            std::fs::write(path, contents).expect("write file");
        }
        let root = ProjectRoot {
            path: dir,
            marker: RootMarker::PyProjectToml,
        };
        let manifest = empty_manifest(root.clone());
        let config = crate::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Unknown,
                package_root: String::new(),
                packages: Vec::new(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let parse = ParseSummary::default();
        scan_config(&PluginContext {
            root: &root,
            config: &config,
            sources: &sources,
            manifest: &manifest,
            parse: &parse,
        })
    }

    /// `(binary, file, line)` triples of every recorded binary usage.
    fn usages(result: &ConfigScanResult) -> Vec<(String, String, Option<u32>)> {
        result
            .binary_usages
            .iter()
            .map(|usage| {
                (
                    usage.binary.clone(),
                    usage.origin.file.clone(),
                    usage.origin.line,
                )
            })
            .collect()
    }

    #[test]
    fn scan_input_paths_lists_existing_candidates_with_sorted_scripts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        std::fs::write(root.join("tox.ini"), "[tox]\n").expect("write tox.ini");
        std::fs::create_dir(root.join("scripts")).expect("create scripts");
        std::fs::create_dir(root.join("scripts/nested")).expect("create nested");
        std::fs::create_dir(root.join("bin")).expect("create bin");
        for name in ["scripts/b.sh", "scripts/a.sh", "bin/run"] {
            std::fs::write(root.join(name), "").expect("write script");
        }

        let paths: Vec<_> = scan_input_paths(root)
            .iter()
            .map(|path| rel_to_root(root, path))
            .collect();

        assert_eq!(
            paths,
            ["tox.ini", "scripts/a.sh", "scripts/b.sh", "bin/run"]
        );
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
            path: dir,
            marker: RootMarker::PyProjectToml,
        };
        let config = crate::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Unknown,
                package_root: String::new(),
                packages: Vec::new(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let manifest = empty_manifest(root.clone());
        let parse = ParseSummary::default();
        let ctx = PluginContext {
            root: &root,
            config: &config,
            sources: &sources,
            manifest: &manifest,
            parse: &parse,
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
            path: dir,
            marker: RootMarker::PyProjectToml,
        };
        let config = crate::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Unknown,
                package_root: String::new(),
                packages: Vec::new(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let manifest = empty_manifest(root.clone());
        let parse = ParseSummary::default();
        let ctx = PluginContext {
            root: &root,
            config: &config,
            sources: &sources,
            manifest: &manifest,
            parse: &parse,
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
            path: dir,
            marker: RootMarker::PyProjectToml,
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
                included_via: Vec::new(),
            });
        let config = crate::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Unknown,
                package_root: String::new(),
                packages: Vec::new(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let parse = ParseSummary::default();
        let ctx = PluginContext {
            root: &root,
            config: &config,
            sources: &sources,
            manifest: &manifest,
            parse: &parse,
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
            path: dir,
            marker: RootMarker::PyProjectToml,
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
                included_via: Vec::new(),
            });
        let config = crate::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Unknown,
                package_root: String::new(),
                packages: Vec::new(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let parse = ParseSummary::default();
        let ctx = PluginContext {
            root: &root,
            config: &config,
            sources: &sources,
            manifest: &manifest,
            parse: &parse,
        };
        let result = scan_config(&ctx);
        assert!(result.used_distributions.contains(&"sphinx".to_owned()));
        assert!(result.used_distributions.contains(&"pytest".to_owned()));
        assert!(result.used_distributions.contains(&"requests".to_owned()));
    }

    #[test]
    fn tox_reads_only_command_words_of_commands_keys() {
        let result = scan_files(&[(
            "tox.ini",
            "[testenv]\ndescription = build a virtualenv and lint\ndeps = black\n    virtualenv\n\
                 commands_pre = ruff check . # black is not run here\ncommands =\n    black --check . ; \
                 pip install build\n    {envpython} -m pytest -q {posargs}\n    {toxinidir}/run.sh \\\n        \
                 mypy src\n[testenv:docs]\nallowlist_externals = virtualenv\ncommands = sphinx-build -b html \
                 docs docs/_build\ncommands_post =\n    py38,!pypy: coverage report\n",
        )]);
        let found = usages(&result);
        assert_eq!(
            found,
            [
                ("tox".to_owned(), "tox.ini".to_owned(), None),
                ("ruff".to_owned(), "tox.ini".to_owned(), Some(5)),
                ("black".to_owned(), "tox.ini".to_owned(), Some(7)),
                ("pytest".to_owned(), "tox.ini".to_owned(), Some(8)),
                ("sphinx-build".to_owned(), "tox.ini".to_owned(), Some(13)),
                ("coverage".to_owned(), "tox.ini".to_owned(), Some(15)),
            ]
        );
        assert!(result.used_distributions.contains(&"sphinx".to_owned()));
    }

    #[test]
    fn shell_scripts_and_local_hooks_read_command_words_only() {
        let result = scan_files(&[
            (
                "scripts/run.sh",
                "#!/usr/bin/env bash\n# pytest is only mentioned here\nuv run ruff check . # black\n\
                     echo mypy\n",
            ),
            (
                "scripts/gen.py",
                "\"\"\"build the docs with sphinx-build\"\"\"\nimport build\n",
            ),
            (
                "bin/release",
                "#!/usr/bin/env python3\nimport subprocess  # black\n",
            ),
            ("bin/lint", "#!/bin/sh -eu\nmypy src\n"),
            (
                ".pre-commit-config.yaml",
                "repos:\n  - repo: local\n    hooks:\n      - id: lint\n        entry: ruff check\n\
                     - repo: https://github.com/psf/black\n    hooks:\n      - id: black\n        \
                     entry: pytest\n",
            ),
        ]);
        let found = usages(&result);
        let scripts = |file: &str| -> Vec<(String, Option<u32>)> {
            found
                .iter()
                .filter(|(_, path, _)| path == file)
                .map(|(binary, _, line)| (binary.clone(), *line))
                .collect()
        };
        assert_eq!(scripts("scripts/run.sh"), [("ruff".to_owned(), Some(3))]);
        assert_eq!(
            scripts("scripts/gen.py"),
            Vec::<(String, Option<u32>)>::new()
        );
        assert_eq!(scripts("bin/release"), Vec::<(String, Option<u32>)>::new());
        assert_eq!(scripts("bin/lint"), [("mypy".to_owned(), Some(2))]);
        assert_eq!(
            scripts(".pre-commit-config.yaml"),
            [
                ("pre-commit".to_owned(), None),
                ("ruff".to_owned(), Some(5)),
                ("black".to_owned(), None),
            ]
        );
    }
}
