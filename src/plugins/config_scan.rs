//! Static config scanning for CLI / dev-tool usage (Phase 1.5 §4.A).

use std::path::{Path, PathBuf};

use ruff_python_ast::Expr;
use ruff_python_ast::visitor::{Visitor, walk_expr};
use rustc_hash::FxHashSet as HashSet;
use serde::{Deserialize, Serialize};

use crate::manifest::literals::{parse_module, string_value};
use crate::manifest::{DependencyContext, normalize_distribution_name};
use crate::path_util::rel_to_root;
use crate::resolver::{VenvIndex, build_binary_map};

use super::commands::{KnownBinary, SourceHits};
use super::config_text::{
    PyprojectDoc, leading_spaces, logical_lines, origin_at, read_root_file, toml_key_line,
};
use super::context::PluginContext;
use super::types::{
    BinaryUsage, CommandTargetRef, ModuleReference, ReferenceOrigin, RequirementsRef,
    ScriptReference, SymbolReference, ToolConfigRef,
};
use super::{ci_installs, script_entries, task_files, tool_plugins};

/// Output from config scanning.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(super) struct ConfigScanResult {
    /// CLI binaries referenced from configuration.
    pub binary_usages: Vec<BinaryUsage>,
    /// Distributions used without a distinct CLI name (themes, tox extras, etc.).
    pub used_distributions: Vec<String>,
    /// Modules named as plugins or callables in config (pytest `-p`, mypy
    /// `plugins`, PDM `call`).
    #[serde(default)]
    pub module_refs: Vec<ModuleReference>,
    /// Distributions tox `deps`, nox `session.install(...)` and CI workflow
    /// installs (#725) install, which provide their binaries the way a
    /// manifest declaration does (#680).
    #[serde(default)]
    pub declared_distributions: Vec<String>,
    /// Binaries remote pre-commit hooks install into pre-commit's own
    /// environment (#680).
    #[serde(default)]
    pub provided_binaries: Vec<String>,
    /// Scripts commands run by path (#797).
    #[serde(default)]
    pub script_entries: Vec<ScriptReference>,
    /// Python code task files and tool configs run (#913).
    #[serde(default)]
    pub command_targets: Vec<CommandTargetRef>,
    /// Classes command options name by path (#1239).
    #[serde(default)]
    pub used_symbols: Vec<SymbolReference>,
    /// Requirements files commands, tox `deps` and nox installs install
    /// from (#1273); read on every run, outside this cache.
    #[serde(default)]
    pub requirement_files: Vec<RequirementsRef>,
    /// Config files commands point a tool at (#1273); read on every run,
    /// outside this cache.
    #[serde(default)]
    pub tool_configs: Vec<ToolConfigRef>,
}

const MKDOCS_CONFIG_NAMES: [&str; 2] = ["mkdocs.yml", "mkdocs.yaml"];
const PRE_COMMIT_CONFIG: &str = ".pre-commit-config.yaml";
const TOX_CONFIG: &str = "tox.ini";
const TOX_TOML: &str = "tox.toml";
const NOX_CONFIG: &str = "noxfile.py";
const SCRIPT_DIRS: [&str; 2] = ["scripts", "bin"];

/// Files [`scan_config`] may read beyond the config and manifest inputs.
///
/// Every existing candidate is listed, not only the ones a scan read, so a
/// cache entry is invalidated when a candidate appears after it was written.
#[must_use]
pub(super) fn scan_input_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = MKDOCS_CONFIG_NAMES
        .into_iter()
        .chain([PRE_COMMIT_CONFIG, TOX_CONFIG, TOX_TOML, NOX_CONFIG])
        .map(|name| root.join(name))
        .filter(|path| path.is_file())
        .collect();
    paths.extend(shell_script_paths(root));
    paths.extend(task_files::input_paths(root));
    paths.extend(tool_plugins::input_paths(root));
    paths.extend(ci_installs::input_paths(root));
    paths.extend(script_entries::input_paths(root));
    paths
}

/// Scan project configuration for dev-tool / CLI usage.
#[must_use]
pub(super) fn scan_config(ctx: &PluginContext<'_>) -> ConfigScanResult {
    let root = ctx.root.path.as_path();
    let mut result = ConfigScanResult::default();
    let mut seen_binaries: HashSet<(String, String)> = HashSet::default();
    let pyproject = PyprojectDoc::load(root);
    let binary_map = build_binary_map(ctx.config, &VenvIndex::default());
    let known = |name: &str| {
        binary_map.contains_key(name)
            || tool_key_to_binary(name).is_some()
            || hook_id_to_binary(name).is_some()
    };

    scan_pyproject_tools(pyproject.as_ref(), &mut result, &mut seen_binaries);
    scan_mkdocs_config(root, &mut result, &mut seen_binaries);
    scan_pre_commit_config(root, &mut result, &mut seen_binaries, &known);
    scan_tox_config(root, ctx, &mut result, &mut seen_binaries, &known);
    scan_pyproject_tox(
        ctx,
        pyproject.as_ref(),
        &mut result,
        &mut seen_binaries,
        &known,
    );
    scan_tox_toml(root, ctx, &mut result, &mut seen_binaries, &known);
    scan_noxfile(root, &mut result);
    scan_shell_scripts(root, &mut result, &mut seen_binaries, &known);
    let hits = task_files::scan(root, pyproject.as_ref(), &known);
    merge_hits(&mut result, &mut seen_binaries, hits);
    let hits = tool_plugins::scan(root, pyproject.as_ref());
    merge_hits(&mut result, &mut seen_binaries, hits);
    let (installed, requirement_files) = ci_installs::scan(root);
    result.declared_distributions.extend(installed);
    result.requirement_files.extend(requirement_files);
    result.script_entries.extend(script_entries::scan(root));

    for list in [
        &mut result.used_distributions,
        &mut result.declared_distributions,
        &mut result.provided_binaries,
    ] {
        list.sort();
        list.dedup();
    }
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
    for (key, value) in tool {
        if key == "isort" && isort_skips_every_file(value) {
            continue;
        }
        if let Some(binary) = tool_key_to_binary(key) {
            push_usage(
                result,
                seen,
                BinaryUsage {
                    binary: binary.to_owned(),
                    origin: doc.origin("tool", key),
                    config_section: true,
                    inferred: false,
                },
            );
        }
    }
}

/// `[tool.isort] skip_glob = ["*"]` keeps editors from running isort; the
/// section disables the tool rather than configuring it (sqlalchemy, #1241).
/// Any other `skip_glob` is a usage as before.
fn isort_skips_every_file(section: &toml::Value) -> bool {
    section
        .get("skip_glob")
        .and_then(toml::Value::as_array)
        .is_some_and(|globs| {
            globs
                .iter()
                .any(|glob| matches!(glob.as_str(), Some("*" | "**" | "**/*")))
        })
}

fn merge_hits(
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    hits: SourceHits,
) {
    for usage in hits.binaries {
        push_usage(result, seen, usage);
    }
    for distribution in &hits.distributions {
        push_distribution(result, distribution);
    }
    result.module_refs.extend(hits.module_refs);
    result.command_targets.extend(hits.command_targets);
    result.declared_distributions.extend(hits.installed);
    result.requirement_files.extend(hits.requirement_files);
    result.tool_configs.extend(hits.tool_configs);
    result.used_symbols.extend(hits.used_symbols);
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
    push_binary(
        result,
        seen,
        "pre-commit",
        ReferenceOrigin {
            file: rel.clone(),
            line: None,
            label: PRE_COMMIT_CONFIG.to_owned(),
        },
    );

    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut hook = PreCommitHook::default();
    for (index, line) in contents.lines().enumerate() {
        let trimmed = strip_yaml_comment(line).trim();
        let item = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim_start();
        if trimmed.starts_with("- ") && starts_pre_commit_item(item) {
            hook.flush(result, seen, known, &rel);
        }
        if let Some(repo) = item.strip_prefix("repo:") {
            hook.local_repo = unquote_yaml_scalar(repo) == "local";
        } else if let Some(hook_id) = item.strip_prefix("id:") {
            hook.binaries
                .extend(hook_id_to_binary(unquote_yaml_scalar(hook_id)));
        } else if let Some(entry) = item.strip_prefix("entry:") {
            hook.entry = Some((unquote_yaml_scalar(entry).to_owned(), index));
        } else if let Some(language) = item.strip_prefix("language:") {
            hook.language = Some(unquote_yaml_scalar(language).to_owned());
        } else if let Some(dependencies) = item.strip_prefix("additional_dependencies:") {
            hook.additional_dependencies = dependencies.trim() != "[]";
        }
    }
    hook.flush(result, seen, known, &rel);
}

/// pre-commit `language`s that run the hook in the project's own environment.
/// With any other language and `additional_dependencies`, pre-commit builds
/// the hook an env with those dependencies (airflow's `language: python`
/// bandit, #1241). A local hook without `language` or without
/// `additional_dependencies` is read as the project's env, which keeps its
/// CHK008: a `language: python` env without them finds its entry on `PATH`.
const PRE_COMMIT_PROJECT_ENV_LANGUAGES: &[&str] =
    &["system", "script", "unsupported", "unsupported_script"];

/// The fields of one `.pre-commit-config.yaml` hook, which may come in any
/// order, and whether its repo is `local`.
#[derive(Default)]
struct PreCommitHook {
    local_repo: bool,
    /// The binary of a known hook `id`.
    binaries: Vec<&'static str>,
    /// `entry` with its 0-based line.
    entry: Option<(String, usize)>,
    language: Option<String>,
    /// `additional_dependencies` is present and not `[]`.
    additional_dependencies: bool,
}

impl PreCommitHook {
    /// Record the hook and reset it. Remote hooks and local hooks with their
    /// own env install their tool into pre-commit's cache, which provides
    /// the binary their config sections refer to (#680); only the other
    /// local hooks run a command from the project's environment, and only
    /// local hooks have an `entry` the project wrote.
    fn flush(
        &mut self,
        result: &mut ConfigScanResult,
        seen: &mut HashSet<(String, String)>,
        known: KnownBinary<'_>,
        rel: &str,
    ) {
        let local_repo = self.local_repo;
        let hook = std::mem::replace(
            self,
            Self {
                local_repo,
                ..Self::default()
            },
        );
        let isolated = !local_repo
            || (hook.additional_dependencies
                && hook
                    .language
                    .as_deref()
                    .is_some_and(|language| !PRE_COMMIT_PROJECT_ENV_LANGUAGES.contains(&language)));
        let origin = ReferenceOrigin {
            file: rel.to_owned(),
            line: None,
            label: PRE_COMMIT_CONFIG.to_owned(),
        };
        for binary in hook.binaries {
            push_isolable(result, seen, binary, &origin, isolated);
        }
        let Some((entry, index)) = hook.entry.filter(|_| local_repo) else {
            return;
        };
        let origin = origin_at(rel, index, "pre-commit entry");
        push_hook_command(result, seen, &entry, known, &origin, isolated);
        result
            .script_entries
            .extend(script_entries::command_entries(
                &entry,
                &origin,
                script_entries::Fallback::None,
            ));
    }
}

/// A hook's binary usage; a hook in pre-commit's own env also provides it.
fn push_isolable(
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    binary: &str,
    origin: &ReferenceOrigin,
    isolated: bool,
) {
    if isolated {
        result.provided_binaries.push(binary.to_owned());
    }
    push_usage(
        result,
        seen,
        BinaryUsage {
            binary: binary.to_owned(),
            origin: origin.clone(),
            config_section: isolated,
            inferred: false,
        },
    );
}

/// A `- key:` list item with a lowercase key (`id`, `repo`) starts the next
/// hook, unlike a block list entry of `additional_dependencies` or `args`
/// (`- bandit==1.7.6`, `- E501:ignore`). A hook written as a bare `-` line
/// with its keys below does not start one, so its fields join the previous
/// hook's.
fn starts_pre_commit_item(item: &str) -> bool {
    item.split_once(':').is_some_and(|(key, _)| {
        !key.is_empty() && key.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_')
    })
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
    scan_tox_contents(ctx, &rel, 0, &contents, result);
    scan_tox_commands(&rel, 0, &contents, result, seen, known);
}

/// pyproject `[tool.tox]`: the `legacy_tox_ini` string is read like `tox.ini`,
/// and the tox 4 native tables like `tox.toml` (#724). The `tox` usage itself
/// comes from the `[tool.tox]` table key.
fn scan_pyproject_tox(
    ctx: &PluginContext<'_>,
    pyproject: Option<&PyprojectDoc>,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    known: KnownBinary<'_>,
) {
    let Some(doc) = pyproject else {
        return;
    };
    let Some(tox) = doc.table("tool.tox") else {
        return;
    };
    if let Some(contents) = tox.get("legacy_tox_ini").and_then(toml::Value::as_str) {
        // TOML drops the newline right after an opening `"""`, so the string
        // starts on the line after the key unless text follows the quotes.
        let key_line = doc.origin("tool.tox", "legacy_tox_ini").line;
        let offset = key_line.map_or(0, |line| {
            let line = usize::try_from(line).unwrap_or(0);
            let key_text = doc.line_text(line).unwrap_or_default().trim_end();
            if key_text.ends_with("\"\"\"") || key_text.ends_with("'''") {
                line
            } else {
                line.saturating_sub(1)
            }
        });
        scan_tox_contents(ctx, &doc.rel, offset, contents, result);
        scan_tox_commands(&doc.rel, offset, contents, result, seen, known);
    }
    scan_native_tox(
        ctx,
        tox,
        &doc.rel,
        "tool.tox",
        doc.text(),
        result,
        seen,
        known,
    );
}

/// `tox.toml`, the tox 4 native config (#724).
fn scan_tox_toml(
    root: &Path,
    ctx: &PluginContext<'_>,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    known: KnownBinary<'_>,
) {
    let Some((rel, text)) = read_root_file(root, TOX_TOML) else {
        return;
    };
    let origin = ReferenceOrigin {
        file: rel.clone(),
        line: None,
        label: TOX_TOML.to_owned(),
    };
    push_binary(result, seen, "tox", origin);
    let Ok(table) = toml::from_str::<toml::Table>(&text) else {
        return;
    };
    scan_native_tox(ctx, &table, &rel, "", &text, result, seen, known);
}

/// `env_run_base`, `env_pkg_base` and `env.<name>` tables of a native tox
/// config: `deps` provide distributions, `extras` mark optional dependencies
/// used and `commands*` run binaries, as in `tox.ini`. `prefix` is the dotted
/// path of `table` (`tool.tox` in pyproject, empty in `tox.toml`).
#[expect(
    clippy::too_many_arguments,
    reason = "shares its caller's result, dedup set and binary lookup across tox sections"
)]
fn scan_native_tox(
    ctx: &PluginContext<'_>,
    table: &toml::Table,
    rel: &str,
    prefix: &str,
    text: &str,
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    known: KnownBinary<'_>,
) {
    let join = |name: &str| {
        if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}.{name}")
        }
    };
    let mut envs: Vec<(String, &toml::Table)> = ["env_run_base", "env_pkg_base"]
        .into_iter()
        .filter_map(|name| Some((join(name), table.get(name)?.as_table()?)))
        .collect();
    if let Some(named) = table.get("env").and_then(toml::Value::as_table) {
        envs.extend(
            named
                .iter()
                .filter_map(|(name, env)| Some((join(&format!("env.{name}")), env.as_table()?))),
        );
    }
    let strings = |value: Option<&toml::Value>| -> Vec<String> {
        value
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    for (path, env) in envs {
        let origin = ReferenceOrigin {
            file: rel.to_owned(),
            line: toml_key_line(text, &path, "deps"),
            label: format!("{path}.deps"),
        };
        for dep in strings(env.get("deps")) {
            push_tox_dependency(&dep, &origin, result);
        }
        for extra in strings(env.get("extras")) {
            mark_optional_extra_dependencies(ctx, &extra, result);
        }
        for key in TOX_COMMAND_KEYS {
            let Some(commands) = env.get(key).and_then(toml::Value::as_array) else {
                continue;
            };
            let origin = ReferenceOrigin {
                file: rel.to_owned(),
                line: toml_key_line(text, &path, key),
                label: format!("{path}.{key}"),
            };
            for command in commands.iter().filter_map(native_tox_command) {
                push_command(
                    result,
                    seen,
                    &expand_tox_substitutions(&command),
                    known,
                    &origin,
                );
            }
        }
    }
}

/// A native tox command is an argv run without a shell, so an argument with
/// `;`, `|` or `&` (`python -c "a; b"`) is dropped before the words reach the
/// shell-aware [`push_command`].
fn native_tox_command(command: &toml::Value) -> Option<String> {
    let Some(argv) = command.as_array() else {
        return command.as_str().map(str::to_owned);
    };
    let words: Vec<&str> = argv
        .iter()
        .filter_map(toml::Value::as_str)
        .filter(|arg| !arg.contains([';', '|', '&']))
        .collect();
    Some(words.join(" "))
}

const TOX_COMMAND_KEYS: [&str; 3] = ["commands", "commands_pre", "commands_post"];

/// Binaries run by `commands` / `commands_pre` / `commands_post` in any
/// section. Other keys (`deps`, `description`, `allowlist_externals`, ...)
/// name tools without running them, so their words are not commands (#494).
#[expect(
    clippy::too_many_arguments,
    reason = "shares its caller's result, dedup set and binary lookup across tox sections"
)]
fn scan_tox_commands(
    rel: &str,
    line_offset: usize,
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
            &origin_at(rel, line_offset + index, "tox commands"),
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
        .replace("{env_bin_dir}/", "")
        .replace("{toxinidir}/", "")
        .replace("{tox_root}/", "");
    let mut depth = 0usize;
    let mut expanded = String::with_capacity(command.len());
    for ch in command.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
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

/// `extras` and `deps` of every section. A line is read as INI reads it, a
/// `key = value` with or without spaces around `=` (`deps=pytest`) or `:`
/// (#1273); a line with neither sets no key and only ends the one before.
fn scan_tox_contents(
    ctx: &PluginContext<'_>,
    rel: &str,
    line_offset: usize,
    contents: &str,
    result: &mut ConfigScanResult,
) {
    let mut current_key: Option<ToxListKey> = None;

    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            current_key = None;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        let origin = || origin_at(rel, line_offset + index, "tox deps");
        if let Some(key) = current_key
            && line.chars().next().is_some_and(char::is_whitespace)
        {
            match key {
                ToxListKey::Deps => push_tox_dependency(trimmed, &origin(), result),
                ToxListKey::Extras => push_tox_extras(ctx, trimmed, result),
            }
            continue;
        }
        current_key = None;
        let Some((key, value)) = trimmed.split_once(['=', ':']) else {
            continue;
        };
        // `key +=` appends to the inherited list; it is read the same way.
        let key = key.trim();
        match key.strip_suffix('+').map_or(key, str::trim_end) {
            "extras" => {
                current_key = Some(ToxListKey::Extras);
                push_tox_extras(ctx, value, result);
            },
            "deps" => {
                current_key = Some(ToxListKey::Deps);
                push_tox_dependency(value, &origin(), result);
            },
            _ => {},
        }
    }
}

fn push_tox_extras(ctx: &PluginContext<'_>, raw: &str, result: &mut ConfigScanResult) {
    for extra in raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        mark_optional_extra_dependencies(ctx, extra, result);
    }
}

/// One `deps` entry: a requirement, or `-r file`, which installs what the
/// file lists (#1273). A `-c file` only constrains versions, and
/// `{[testenv]deps}` substitutes another section's entries, read there.
fn push_tox_dependency(raw: &str, origin: &ReferenceOrigin, result: &mut ConfigScanResult) {
    let entry = strip_tox_factors(raw.trim());
    let mut words = entry.split_whitespace();
    if let Some(first) = words.next()
        && let Some(file) = ci_installs::requirement_option(first, &mut words)
    {
        result.requirement_files.push(RequirementsRef {
            path: tox_root_path(&file),
            origin: origin.clone(),
            missing_ok: false,
        });
        return;
    }
    if let Some(name) = extract_requirement_name(entry) {
        push_distribution(result, &name);
        // `-c file`, `{[testenv]deps}` and paths are not distributions.
        if is_distribution_name(&name) {
            result.declared_distributions.push(name);
        }
    }
}

/// A tox path with its `{toxinidir}` / `{tox_root}` prefix dropped: tox
/// runs pip from that directory, the project root.
fn tox_root_path(path: &str) -> String {
    ["{toxinidir}", "{tox_root}"]
        .iter()
        .find_map(|prefix| path.strip_prefix(prefix))
        .map_or(path, |rest| rest.trim_start_matches(['/', '\\']))
        .to_owned()
}

/// A PEP 508 project name (letters, digits, `-`, `_`, `.`).
pub(super) fn is_distribution_name(name: &str) -> bool {
    name.starts_with(|ch: char| ch.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// Requirement literals passed to `session.install(...)` in `noxfile.py`.
/// The file is parsed, never run; options (`-e`, `-r file`) and non-literal
/// arguments are skipped.
fn scan_noxfile(root: &Path, result: &mut ConfigScanResult) {
    let Ok(contents) = std::fs::read_to_string(root.join(NOX_CONFIG)) else {
        return;
    };
    let Some(suite) = parse_module(&contents) else {
        return;
    };
    let mut visitor = SessionInstallVisitor::default();
    visitor.visit_body(&suite);
    let origin = ReferenceOrigin {
        file: NOX_CONFIG.to_owned(),
        line: None,
        label: "session.install".to_owned(),
    };
    for call in &visitor.calls {
        let mut args = call.iter().map(|arg| arg.as_deref());
        while let Some(arg) = args.next() {
            let Some(requirement) = arg else {
                continue;
            };
            // A computed path is read as empty, an unread file (ADR 0005).
            let mut rest = args.by_ref().map(Option::unwrap_or_default);
            if let Some(file) = ci_installs::requirement_option(requirement, &mut rest) {
                result.requirement_files.push(RequirementsRef {
                    path: file,
                    origin: origin.clone(),
                    missing_ok: false,
                });
                continue;
            }
            if requirement.starts_with('-') {
                // `"-c", "constraints.txt"`: the option's value is a path, not a name.
                if matches!(requirement, "-c" | "--constraint" | "-e" | "--editable") {
                    args.next();
                }
                continue;
            }
            if let Some(name) = extract_requirement_name(requirement)
                && is_distribution_name(&name)
            {
                result.declared_distributions.push(name);
            }
        }
    }
}

#[derive(Default)]
struct SessionInstallVisitor {
    /// The positional arguments of each call, `None` where not a literal.
    calls: Vec<Vec<Option<String>>>,
}

impl<'a> Visitor<'a> for SessionInstallVisitor {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Call(call) = expr
            && let Expr::Attribute(attribute) = &*call.func
            && attribute.attr.as_str() == "install"
            && matches!(&*attribute.value, Expr::Name(name) if name.id.as_str() == "session")
        {
            self.calls
                .push(call.arguments.args.iter().map(string_value).collect());
        }
        walk_expr(self, expr);
    }
}

pub(super) fn extract_requirement_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let end = trimmed
        .find(['[', ';', '<', '>', '=', '!', '~', '@', ' '])
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
pub(super) fn is_shell_script(path: &Path, contents: &str) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => ["sh", "bash", "zsh", "ksh"]
            .iter()
            .any(|shell| ext.eq_ignore_ascii_case(shell)),
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
pub(super) fn strip_shell_comment(line: &str) -> &str {
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
    push_hook_command(result, seen, command, known, origin, false);
}

/// [`push_command`] for a pre-commit hook `entry`, which pre-commit's own env
/// provides when `isolated`.
#[expect(
    clippy::too_many_arguments,
    reason = "push_command's arguments plus the hook's env"
)]
fn push_hook_command(
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    command: &str,
    known: KnownBinary<'_>,
    origin: &ReferenceOrigin,
    isolated: bool,
) {
    let mut hits = SourceHits::default();
    hits.push_command(command, known, origin);
    for usage in &mut hits.binaries {
        if usage.binary == "sphinx-build" {
            push_distribution(result, "sphinx");
        }
        if isolated {
            result.provided_binaries.push(usage.binary.clone());
            usage.config_section = true;
        }
    }
    merge_hits(result, seen, hits);
}

fn push_binary(
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    binary: &str,
    origin: ReferenceOrigin,
) {
    push_usage(
        result,
        seen,
        BinaryUsage {
            binary: binary.to_owned(),
            origin,
            config_section: false,
            inferred: false,
        },
    );
}

fn push_usage(
    result: &mut ConfigScanResult,
    seen: &mut HashSet<(String, String)>,
    usage: BinaryUsage,
) {
    let key = (
        usage.binary.clone(),
        // A remote and a local hook share an origin; keep both.
        format!(
            "{}:{}:{}",
            usage.origin.file,
            usage.origin.line.unwrap_or_default(),
            usage.config_section
        ),
    );
    if seen.insert(key) {
        result.binary_usages.push(usage);
    }
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
    use crate::sources::{DiscoveredSources, LayoutInfo};

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
        let config = crate::config::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo::default(),
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
        let config = crate::config::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo::default(),
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
        let config = crate::config::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo::default(),
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
                unportable_key: None,
            });
        let config = crate::config::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo::default(),
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
                unportable_key: None,
            });
        let config = crate::config::default_config();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo::default(),
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
    fn pyproject_legacy_tox_ini_reads_deps_and_commands() {
        let result = scan_files(&[(
            "pyproject.toml",
            "[project]\nname = \"pkg\"\n[tool.tox]\nlegacy_tox_ini = \"\"\"\n[testenv:pre-commit]\n\
             deps = pre-commit\ncommands = pre-commit run --all-files\n\"\"\"\n",
        )]);
        assert_eq!(
            usages(&result),
            [
                ("tox".to_owned(), "pyproject.toml".to_owned(), Some(3)),
                (
                    "pre-commit".to_owned(),
                    "pyproject.toml".to_owned(),
                    Some(7)
                ),
            ]
        );
        assert_eq!(result.declared_distributions, ["pre-commit"]);
    }

    #[test]
    fn native_tox_tables_read_deps_and_commands() {
        let native = "[env_run_base]\ndeps = [\"pytest\", \"-r requirements.txt\"]\n\
                      commands = [[\"pytest\", \"{posargs}\"]]\n[env.lint]\ndeps = [\"ruff\"]\n\
                      commands_pre = [[\"ruff\", \"check\", \".\"], \
                      [\"python\", \"-c\", \"import a; black()\"]]\n";
        let toml_result = scan_files(&[("tox.toml", native)]);
        assert_eq!(
            usages(&toml_result),
            [
                ("tox".to_owned(), "tox.toml".to_owned(), None),
                ("pytest".to_owned(), "tox.toml".to_owned(), Some(3)),
                ("ruff".to_owned(), "tox.toml".to_owned(), Some(6)),
            ]
        );
        assert_eq!(toml_result.declared_distributions, ["pytest", "ruff"]);

        let pyproject = format!(
            "[project]\nname = \"pkg\"\n{}",
            native
                .replace("[env_run_base]", "[tool.tox.env_run_base]")
                .replace("[env.lint]", "[tool.tox.env.lint]")
        );
        let pyproject_result = scan_files(&[("pyproject.toml", pyproject.as_str())]);
        assert_eq!(
            usages(&pyproject_result),
            [
                // No `[tool.tox]` header line to point at.
                ("tox".to_owned(), "pyproject.toml".to_owned(), None),
                ("pytest".to_owned(), "pyproject.toml".to_owned(), Some(5)),
                ("ruff".to_owned(), "pyproject.toml".to_owned(), Some(8)),
            ]
        );
        assert_eq!(pyproject_result.declared_distributions, ["pytest", "ruff"]);
    }

    #[test]
    fn tox_commands_keep_their_block_across_blank_lines_only() {
        let result = scan_files(&[(
            "tox.ini",
            "[testenv]\ncommands =\n    ruff check .\n\n    pytest tests/test_a.py::test_x\n\
                 commands_post = coverage report\n[testenv:lint]\ncommands = {env:RUNNER:} mypy src\n",
        )]);
        assert_eq!(
            usages(&result),
            [
                ("tox".to_owned(), "tox.ini".to_owned(), None),
                ("ruff".to_owned(), "tox.ini".to_owned(), Some(3)),
                ("pytest".to_owned(), "tox.ini".to_owned(), Some(5)),
                ("coverage".to_owned(), "tox.ini".to_owned(), Some(6)),
                ("mypy".to_owned(), "tox.ini".to_owned(), Some(8)),
            ]
        );
        assert!(!result.used_distributions.contains(&"sphinx".to_owned()));
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
                "#!/usr/bin/env python3\nblack = subprocess.run([\"black\", \".\"])\n",
            ),
            ("bin/lint", "#!/bin/sh -eu\nmypy src\n"),
            ("bin/check", "#!/usr/bin/env -S bash -e\nruff check\n"),
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
        assert_eq!(scripts("bin/check"), [("ruff".to_owned(), Some(2))]);
        assert_eq!(
            scripts(".pre-commit-config.yaml"),
            [
                ("pre-commit".to_owned(), None),
                ("ruff".to_owned(), Some(5)),
                ("black".to_owned(), None),
            ]
        );
    }

    #[test]
    fn remote_hooks_provide_their_binaries_to_config_sections_only() {
        let result = scan_files(&[(
            ".pre-commit-config.yaml",
            "repos:\n  - repo: local\n    hooks:\n      - id: lint\n        entry: ruff check\n\
                 - repo: https://github.com/pre-commit/mirrors-mypy\n    hooks:\n      - id: mypy\n\
                 - repo: local\n    hooks:\n      - id: mypy\n        entry: python -m mypy\n\
                       - id: black\n        entry: black .\n",
        )]);
        assert_eq!(result.provided_binaries, ["mypy"]);
        let sections: Vec<_> = result
            .binary_usages
            .iter()
            .map(|usage| (usage.binary.as_str(), usage.config_section))
            .collect();
        assert!(sections.contains(&("mypy", true)));
        assert!(sections.contains(&("mypy", false)));
        assert!(sections.contains(&("black", false)));
        assert!(!sections.contains(&("black", true)));
        assert!(sections.contains(&("ruff", false)));
    }

    #[test]
    fn local_hooks_with_their_own_language_env_provide_their_binaries() {
        let result = scan_files(&[(
            ".pre-commit-config.yaml",
            // Block list items before `language` stay in the same hook.
            "repos:\n  - repo: local\n    hooks:\n      - id: bandit\n        name: bandit\n\
                         entry: bandit -r airflow\n        additional_dependencies:\n\
                           - bandit==1.7.6\n        args:\n          - E501:ignore\n\
                         language: python\n\
                       - entry: ruff check\n        id: lint\n        language: system\n\
                       - id: mypy\n        entry: mypy\n        language: python\n        \
                         additional_dependencies: []\n",
        )]);
        assert_eq!(result.provided_binaries, ["bandit"]);
        let sections: Vec<_> = result
            .binary_usages
            .iter()
            .map(|usage| {
                (
                    usage.binary.as_str(),
                    usage.origin.line,
                    usage.config_section,
                )
            })
            .collect();
        assert_eq!(
            sections,
            [
                ("pre-commit", None, false),
                ("bandit", None, true),
                ("bandit", Some(6), true),
                ("ruff", Some(12), false),
                ("mypy", None, false),
                ("mypy", Some(16), false),
            ]
        );
    }

    /// The hook isolation `docs/dev/formal/binary_providers_model.py`
    /// (`hook_usages`) ports: remote / local × every `language` the
    /// project-env list names, an own-env one and none × no / empty /
    /// non-empty `additional_dependencies`. An isolated hook provides its
    /// binary and makes its usages config sections; a local hook's `entry`
    /// is a usage too.
    #[test]
    fn hook_isolation_follows_the_formal_model() {
        let languages = [
            None,
            Some("system"),
            Some("script"),
            Some("unsupported"),
            Some("unsupported_script"),
            Some("python"),
            Some("node"),
        ];
        for local in [false, true] {
            for language in languages {
                for additional in [None, Some("[]"), Some("[mypy-extensions]")] {
                    let repo = if local {
                        "local"
                    } else {
                        "https://github.com/pre-commit/mirrors-mypy"
                    };
                    let language_line = language
                        .map(|language| format!("        language: {language}\n"))
                        .unwrap_or_default();
                    let additional_line = additional
                        .map(|deps| format!("        additional_dependencies: {deps}\n"))
                        .unwrap_or_default();
                    let hook = format!(
                        "repos:\n  - repo: {repo}\n    hooks:\n      - id: mypy\n        \
                         entry: mypy\n{language_line}{additional_line}"
                    );
                    let result = scan_files(&[(".pre-commit-config.yaml", &hook)]);
                    let isolated = !local
                        || (additional.is_some_and(|deps| deps != "[]")
                            && language.is_some_and(|language| {
                                !PRE_COMMIT_PROJECT_ENV_LANGUAGES.contains(&language)
                            }));
                    let label = format!("local={local} language={language:?} deps={additional:?}");
                    let provided: &[&str] = if isolated { &["mypy"] } else { &[] };
                    assert_eq!(result.provided_binaries, provided, "{label}");
                    let mypy: Vec<_> = result
                        .binary_usages
                        .iter()
                        .filter(|usage| usage.binary == "mypy")
                        .map(|usage| (usage.origin.line.is_some(), usage.config_section))
                        .collect();
                    let mut expected = vec![(false, isolated)];
                    if local {
                        expected.push((true, isolated));
                    }
                    assert_eq!(mypy, expected, "{label}");
                }
            }
        }
    }

    #[test]
    fn isort_section_that_skips_every_file_is_not_a_usage() {
        let binaries = |pyproject: &str| -> Vec<String> {
            let result = scan_files(&[("pyproject.toml", pyproject)]);
            result
                .binary_usages
                .iter()
                .map(|usage| usage.binary.clone())
                .collect()
        };
        assert_eq!(
            binaries("[tool.isort]\nskip_glob = [\"*\"]\n\n[tool.black]\n"),
            ["black"]
        );
        assert_eq!(
            binaries("[tool.isort]\nskip_glob = [\"docs/*\"]\n"),
            ["isort"]
        );
    }

    #[test]
    fn tox_deps_and_nox_installs_declare_distributions() {
        let result = scan_files(&[
            (
                "tox.ini",
                "[testenv]\ndeps =\n    pytest>=7\n    -r requirements/test.txt\n    \
                     {[base]deps}\n    Sphinx[docs]\n    py311: coverage\ncommands = pytest\n",
            ),
            (
                "noxfile.py",
                "import nox\n\n@nox.session\ndef lint(session):\n    \
                     session.install(\"flake8\", \"-e\", \"tools\", \"black~=24.1\")\n    \
                     session.install(\"-r\", \"requirements.txt\", \"isort@git+https://x\")\n    \
                     session.run(\"flake8\")\n",
            ),
        ]);
        assert_eq!(
            result.declared_distributions,
            ["black", "coverage", "flake8", "isort", "pytest", "sphinx"]
        );
        let files: Vec<(&str, &str)> = result
            .requirement_files
            .iter()
            .map(|reference| (reference.path.as_str(), reference.origin.file.as_str()))
            .collect();
        assert_eq!(
            files,
            [
                ("requirements/test.txt", "tox.ini"),
                ("requirements.txt", "noxfile.py")
            ]
        );
    }

    /// INI reads `deps=x`, `deps= x` and `deps: x` as `deps = x`, and
    /// `-r` takes its file with or without a space or `{toxinidir}`
    /// (#1273). A `-c` file only constrains, and a computed nox path is
    /// kept empty, an unread file.
    #[test]
    fn tox_keys_without_spaces_and_requirement_references() {
        let result = scan_files(&[
            (
                "tox.ini",
                "[testenv]\ndeps=flake8\n\n[testenv:a]\ndeps= pytest\n    -r{toxinidir}/ci/a.txt\n\
                 [testenv:b]\ndeps: mypy\n    -c constraints.txt\n    --requirement=ci/b.txt\n\
                 [testenv:c]\ndeps +=\n    -r ci/c.txt\n",
            ),
            (
                "noxfile.py",
                "import nox\n\n@nox.session\ndef t(session):\n    \
                 session.install(\"-r\", REQS)\n    session.install(\"-rci/d.txt\")\n",
            ),
        ]);
        assert_eq!(result.declared_distributions, ["flake8", "mypy", "pytest"]);
        let files: Vec<&str> = result
            .requirement_files
            .iter()
            .map(|reference| reference.path.as_str())
            .collect();
        assert_eq!(files, ["ci/a.txt", "ci/b.txt", "ci/c.txt", "", "ci/d.txt"]);
        let lines: Vec<Option<u32>> = result.requirement_files[..3]
            .iter()
            .map(|reference| reference.origin.line)
            .collect();
        assert_eq!(lines, [Some(6), Some(10), Some(13)]);
    }
}
