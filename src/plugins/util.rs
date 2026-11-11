//! Shared helpers for plugin extractors.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::CharIndices;
use std::sync::LazyLock;

use regex::Regex;
use ruff_text_size::TextSize;
use toml::Value;

use crate::manifest::ManifestError;
use crate::manifest::literals::LiteralScan;
use crate::manifest::util::{path_is_within_root, read_to_string};
use crate::parser::{LineIndex, ParseSeverity, ParsedModule};
use crate::path_util::{join_rel, rel_to_root};
use crate::sources::path_to_module;

use super::commands::is_dotted_identifier;
use super::context::PluginContext;
use super::error::PluginsError;
use super::types::{
    BinaryUsage, ModuleReference, PluginContribution, ReferenceOrigin, SymbolReference,
};

/// INI section key-value pairs.
pub(super) type IniSection = BTreeMap<String, String>;

/// Line of the earliest decorator in `module` whose normalized name and call
/// form match.
///
/// Decorator sites come from the step 6 AST walk in visit order, so nested
/// definitions can precede later top-level ones; take the minimum line to keep
/// reporting the first occurrence in the file.
///
/// A module with a syntax error has no decorator sites, so one unparsable
/// line would hide every decorator in the file; fall back to
/// [`text_decorator_line`] over the source text with the same predicate.
fn decorator_line(
    root: &Path,
    module: &ParsedModule,
    matches: fn(&str, bool) -> bool,
) -> Option<u32> {
    if module
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == ParseSeverity::Error)
    {
        let contents = std::fs::read_to_string(root.join(&module.path)).ok()?;
        return text_decorator_line(&contents, matches);
    }
    module
        .decorator_sites
        .iter()
        .filter(|site| matches(&site.name, site.is_call))
        .map(|site| site.line)
        .min()
}

/// Push a module reference for each Python source with a matching decorator.
pub(super) fn push_decorated_modules(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    is_decorator: fn(&str, bool) -> bool,
    label: &str,
) {
    for module in &ctx.parse.modules {
        let Some(line) = decorator_line(&ctx.root.path, module, is_decorator) else {
            continue;
        };
        push_decorated_module(ctx, contrib, &module.path, line, label);
    }
}

fn push_decorated_module(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    file: &str,
    line: u32,
    label: &str,
) {
    let Some(module) = path_to_module(file, &ctx.sources.layout) else {
        return;
    };
    contrib.module_refs.push(ModuleReference {
        module,
        origin: ReferenceOrigin {
            file: file.to_owned(),
            line: Some(line),
            label: label.to_owned(),
        },
    });
}

/// Push the app a `FLASK_APP` / `flask --app` / `celery -A` value names.
///
/// `module:attr` is a symbol reference; Flask's `module:factory(args)` names
/// the factory. A bare dotted module is a module reference: both tools import
/// it and look up `app_names` in it, so those are used (#851). Shell quotes
/// around the value are dropped. Returns the bare module, if any.
pub(super) fn push_app_ref<'a>(
    contrib: &mut PluginContribution,
    value: &'a str,
    origin: &ReferenceOrigin,
    app_names: &[&str],
) -> Option<&'a str> {
    let value = value.trim().trim_matches(['"', '\'']);
    if let Some((module, symbol)) = parse_module_symbol(value) {
        let symbol = symbol
            .split_once('(')
            .map_or(symbol.as_str(), |(name, _)| name);
        contrib.symbol_refs.push(SymbolReference {
            module,
            symbol: symbol.trim().to_owned(),
            origin: origin.clone(),
        });
        return None;
    }
    if !is_dotted_identifier(value) {
        return None;
    }
    for name in app_names {
        push_used_symbol(contrib, value, name, origin);
    }
    contrib.module_refs.push(ModuleReference {
        module: value.to_owned(),
        origin: origin.clone(),
    });
    Some(value)
}

/// Record that a tool looks `symbol` up by name in `module`.
pub(super) fn push_used_symbol(
    contrib: &mut PluginContribution,
    module: &str,
    symbol: &str,
    origin: &ReferenceOrigin,
) {
    contrib.used_symbols.push(SymbolReference {
        module: module.to_owned(),
        symbol: symbol.to_owned(),
        origin: origin.clone(),
    });
}

/// Record a CLI binary usage.
pub(super) fn push_binary(contrib: &mut PluginContribution, binary: &str, origin: ReferenceOrigin) {
    contrib.binary_usages.push(BinaryUsage {
        binary: binary.to_owned(),
        origin,
        config_section: false,
        inferred: false,
    });
}

/// Text fallback for [`decorator_line`] when a module failed to parse.
///
/// Each line is normalized the way the parse path normalizes a decorator
/// expression, so the same predicate decides both paths.
fn text_decorator_line(contents: &str, matches: fn(&str, bool) -> bool) -> Option<u32> {
    contents.lines().enumerate().find_map(|(index, line)| {
        let (name, is_call) = text_decorator(line)?;
        if matches(&name, is_call) {
            u32::try_from(index + 1).ok()
        } else {
            None
        }
    })
}

/// Dotted name and call form of a one-line decorator: `@apps[0].route("/")`
/// gives `("apps[].route", true)`. `None` for anything the parse path would not
/// normalize either, such as `@a if b else c`.
fn text_decorator(line: &str) -> Option<(String, bool)> {
    let mut rest = line.trim_start().strip_prefix('@')?;
    let mut name = String::new();
    loop {
        let (ident, tail) = split_identifier(rest.trim_start())?;
        name.push_str(ident);
        let (tail, is_call) = skip_trailers(tail, &mut name)?;
        match tail.strip_prefix('.') {
            Some(next) => {
                name.push('.');
                rest = next;
            },
            None => return (tail.is_empty() || tail.starts_with('#')).then_some((name, is_call)),
        }
    }
}

fn split_identifier(text: &str) -> Option<(&str, &str)> {
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    if end == 0 {
        None
    } else {
        Some(text.split_at(end))
    }
}

/// Consume the subscripts and calls after one name segment. A call still open
/// at the end of the line has its arguments on the following lines.
fn skip_trailers<'a>(mut text: &'a str, name: &mut String) -> Option<(&'a str, bool)> {
    let mut is_call = false;
    loop {
        text = text.trim_start();
        let opener = text.chars().next();
        match opener {
            Some('[') => {
                name.push_str("[]");
                is_call = false;
            },
            Some('(') => is_call = true,
            _ => return Some((text, is_call)),
        }
        match skip_group(text) {
            Some(tail) => text = tail,
            None if is_call => return Some(("", true)),
            None => return None,
        }
    }
}

/// Text after the bracket group that `text` opens, or `None` when the group
/// does not close on this line.
fn skip_group(text: &str) -> Option<&str> {
    let mut depth = 0_u32;
    let mut chars = text.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' | '\'' => skip_string(&mut chars, c)?,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return text.get(index + 1..);
                }
            },
            '#' => return None,
            _ => {},
        }
    }
    None
}

fn skip_string(chars: &mut CharIndices<'_>, quote: char) -> Option<()> {
    while let Some((_, c)) = chars.next() {
        if c == quote {
            return Some(());
        }
        if c == '\\' {
            chars.next();
        }
    }
    None
}

/// Split a normalized decorator name into its receiver and final attribute.
pub(super) fn decorator_suffix(name: &str) -> (Option<&str>, &str) {
    name.rsplit_once('.')
        .map_or((None, name), |(receiver, suffix)| (Some(receiver), suffix))
}

/// Read a single INI section from a config file.
pub(super) fn read_ini_section(
    path: &Path,
    section_name: &str,
) -> Result<IniSection, PluginsError> {
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
pub(super) fn read_pyproject_table(path: &Path) -> Result<toml::Table, PluginsError> {
    let contents = read_to_string(path).map_err(manifest_io_error)?;
    toml::from_str(&contents).map_err(|source| PluginsError::InvalidConfig {
        path: rel_to_root(path.parent().unwrap_or(path), path),
        detail: source.to_string(),
    })
}

/// Parse one INI section from file contents.
pub(super) fn parse_ini_section(contents: &str, section_name: &str) -> IniSection {
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

/// Return the pytest options table in `pyproject.toml` with its label.
///
/// pytest ≥ 9 reads native `[tool.pytest]` when it holds keys besides
/// `ini_options` (and rejects using both), else `[tool.pytest.ini_options]`.
pub(super) fn pytest_options_from_pyproject(
    table: &toml::Table,
) -> Option<(&toml::Table, &'static str)> {
    let pytest = table
        .get("tool")
        .and_then(Value::as_table)
        .and_then(|tool| tool.get("pytest"))
        .and_then(Value::as_table)?;
    if pytest.keys().any(|key| key != "ini_options") {
        return Some((pytest, "tool.pytest"));
    }
    pytest
        .get("ini_options")
        .and_then(Value::as_table)
        .map(|options| (options, "tool.pytest.ini_options"))
}

/// Return the `[tool.uvicorn]` table when present.
pub(super) fn uvicorn_tool_from_pyproject(table: &toml::Table) -> Option<&toml::Table> {
    table
        .get("tool")
        .and_then(Value::as_table)
        .and_then(|tool| tool.get("uvicorn"))
        .and_then(Value::as_table)
}

/// Parse a comma- or newline-separated pytest path list.
pub(super) fn parse_path_list(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Build test file glob patterns from pytest options.
pub(super) fn pytest_test_globs(testpaths: &[String], python_files: &[String]) -> Vec<String> {
    let file_patterns = if python_files.is_empty() {
        vec!["test_*.py".to_owned(), "*_test.py".to_owned()]
    } else {
        python_files.to_vec()
    };

    // Without `testpaths` pytest collects from the rootdir down, which is how
    // in-package suites (`acme/tests/`) are found.
    let roots = if testpaths.is_empty() {
        vec![String::new()]
    } else {
        testpaths.to_vec()
    };

    let mut globs = Vec::new();
    for root in roots {
        let normalized = root.trim_start_matches("./").trim_end_matches('/');
        let prefix = if normalized.is_empty() || normalized == "." {
            String::new()
        } else {
            format!("{normalized}/")
        };
        for pattern in &file_patterns {
            globs.push(format!("{prefix}**/{pattern}"));
        }
    }
    globs
}

/// `rel` appended to root-relative `base`, with `.` and `..` folded; `None`
/// when it climbs out of the root or is absolute (`/x`, `\\x`, `C:...`),
/// which names no place under it. Either separator is read, as a Windows
/// config or script may use `\`.
pub(super) fn join_lexical(base: &str, rel: &str) -> Option<String> {
    if rel.starts_with(['/', '\\']) || rel.get(1..2) == Some(":") {
        return None;
    }
    let mut parts: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => {},
            ".." => {
                parts.pop()?;
            },
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The root, then each workspace member: the directories a framework's
/// config file is looked for in.
pub(super) fn project_dirs<'a>(ctx: &PluginContext<'a>) -> impl Iterator<Item = &'a str> {
    std::iter::once("").chain(
        ctx.sources
            .layout
            .members
            .iter()
            .map(|member| member.path.as_str()),
    )
}

/// The inventory's context for root-relative `path`; a file it lacks gets
/// the one its path implies, as discovery would give it.
pub(super) fn file_context(ctx: &PluginContext<'_>, path: &str) -> crate::sources::FileContext {
    ctx.sources
        .files
        .iter()
        .find(|file| file.path == path)
        .map_or_else(
            || crate::sources::assign_file_context(path),
            |file| file.context,
        )
}

/// Discovered `.py` files of `module`: its own file first, then every file
/// under it when it is a package. Empty when it names no discovered file.
pub(super) fn module_files<'a>(
    ctx: &PluginContext<'a>,
    module: &str,
) -> Vec<&'a crate::sources::DiscoveredFile> {
    let sources = ctx.sources;
    let known: BTreeSet<String> = sources
        .python_files()
        .map(|file| file.path.clone())
        .collect();
    let Some(path) = crate::entry::resolve_module_to_path(module, &sources.layout, &known) else {
        return Vec::new();
    };
    let package = path.strip_suffix("/__init__.py");
    let own = sources.python_files().filter(|file| file.path == path);
    let rest = sources.python_files().filter(|file| {
        file.path != path
            && package.is_some_and(|dir| {
                file.path
                    .strip_prefix(dir)
                    .is_some_and(|rest| rest.starts_with('/'))
            })
    });
    own.chain(rest).collect()
}

/// The file of `module`, or with `tree` every file of its package, with
/// their context: the inventory's when it has them, else on disk from `dir`
/// or `dir/src`, as a framework that imports it by name finds it. Files the
/// layout left out of the inventory then join it as plugin entries (#932).
pub(super) fn module_paths(
    ctx: &PluginContext<'_>,
    dir: &str,
    module: &str,
    tree: bool,
) -> Vec<(String, crate::sources::FileContext)> {
    let known = module_files(ctx, module);
    if let Some(first) = known.first() {
        // The module's own file sorts before the rest of its package.
        let files = if tree {
            known.as_slice()
        } else {
            std::slice::from_ref(first)
        };
        return files
            .iter()
            .map(|file| (file.path.clone(), file.context))
            .collect();
    }
    let root = ctx.root.path.as_path();
    let rel = module.replace('.', "/");
    for base in [dir.to_owned(), join_rel(dir, "src")] {
        let file = join_rel(&base, &format!("{rel}.py"));
        if root.join(&file).is_file() {
            let context = crate::sources::assign_file_context(&file);
            return vec![(file, context)];
        }
        let package = join_rel(&base, &rel);
        // A tree is walked as `pkgutil` walks a package's `__path__`, which
        // a namespace package (no `__init__.py`) has too.
        let is_package = root.join(&package).join("__init__.py").is_file();
        if is_package || (tree && root.join(&package).is_dir()) {
            if !tree {
                let init = join_rel(&package, "__init__.py");
                let context = crate::sources::assign_file_context(&init);
                return vec![(init, context)];
            }
            let mut files = BTreeSet::new();
            walk_py(root, &package, &mut files);
            return files
                .into_iter()
                .map(|file| {
                    let context = crate::sources::assign_file_context(&file);
                    (file, context)
                })
                .collect();
        }
    }
    Vec::new()
}

/// The `.py` files under root-relative `dir`, as a framework walking a
/// folder or package finds them. Hidden directories, `__pycache__` and
/// symlinked directories are not entered; discovery still applies its
/// exclusions to what is found.
pub(super) fn walk_py(root: &Path, dir: &str, out: &mut BTreeSet<String>) {
    walk_py_at(root, dir, 0, out);
}

fn walk_py_at(root: &Path, dir: &str, depth: usize, out: &mut BTreeSet<String>) {
    const MAX_DEPTH: usize = 16;
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = if dir.is_empty() {
            name.clone()
        } else {
            format!("{dir}/{name}")
        };
        if kind.is_dir() {
            if depth < MAX_DEPTH && !name.starts_with('.') && name != "__pycache__" {
                walk_py_at(root, &path, depth + 1, out);
            }
        } else if is_py_path(&name) {
            out.insert(path);
        }
    }
}

/// Whether `path` ends in `.py`, as written: the tools load the path they
/// are given.
pub(super) fn is_py_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|extension| extension == "py")
}

/// Convert a dotted module path to a root-relative `.py` file path.
pub(super) fn module_to_py_path(module: &str) -> String {
    format!("{}.py", module.replace('.', "/"))
}

/// `DJANGO_SETTINGS_MODULE` set to a string literal, by `setdefault` or by
/// assigning `os.environ[...]`, with the 1-based line of its value.
pub(super) fn extract_django_settings_module(contents: &str) -> Option<(String, u32)> {
    let caps = DJANGO_SETTINGS_RE.captures(contents)?;
    let value = caps.get(1).or_else(|| caps.get(2))?;
    let start = TextSize::try_from(value.start()).ok()?;
    Some((
        value.as_str().to_owned(),
        LineIndex::new(contents).line(start),
    ))
}

/// The 1-based line where `DJANGO_SETTINGS_MODULE` is set, by `setdefault` or
/// by assigning `os.environ[...]`, whatever the value: a caller that read no
/// literal with [`extract_django_settings_module`] knows the value is not one.
pub(super) fn django_settings_module_set_line(contents: &str) -> Option<u32> {
    let found = DJANGO_SETTINGS_SET_RE.find(contents)?;
    let start = TextSize::try_from(found.start()).ok()?;
    Some(LineIndex::new(contents).line(start))
}

/// Parse `module:symbol` from a uvicorn-style target string.
pub(crate) fn parse_module_symbol(value: &str) -> Option<(String, String)> {
    let trimmed = value.trim();
    let (module, symbol) = trimmed.split_once(':')?;
    if module.is_empty() || symbol.is_empty() {
        return None;
    }
    Some((module.to_owned(), symbol.to_owned()))
}

/// Parse `uvicorn pkg.module:app` from a script target string.
pub(crate) fn parse_uvicorn_script_target(value: &str) -> Option<(String, String)> {
    UVICORN_SCRIPT_RE.captures(value).and_then(|caps| {
        let module = caps.get(1)?.as_str().to_owned();
        let symbol = caps.get(2)?.as_str().to_owned();
        Some((module, symbol))
    })
}

#[expect(
    clippy::expect_used,
    reason = "the pattern is a literal; `Regex::new` fails only on a typo, which every run hits"
)]
static DJANGO_SETTINGS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"setdefault\s*\(\s*["']DJANGO_SETTINGS_MODULE["']\s*,\s*["']([^"']+)["']"#,
        r#"|environ\s*\[\s*["']DJANGO_SETTINGS_MODULE["']\s*\]\s*=\s*["']([^"']+)["']"#,
    ))
    .expect("valid django settings regex")
});

#[expect(
    clippy::expect_used,
    reason = "the pattern is a literal; `Regex::new` fails only on a typo, which every run hits"
)]
static DJANGO_SETTINGS_SET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"setdefault\s*\(\s*["']DJANGO_SETTINGS_MODULE["']"#,
        r#"|environ\s*\[\s*["']DJANGO_SETTINGS_MODULE["']\s*\]\s*=[^=]"#,
    ))
    .expect("valid django settings set regex")
});

#[expect(
    clippy::expect_used,
    reason = "the pattern is a literal; `Regex::new` fails only on a typo, which every run hits"
)]
static UVICORN_SCRIPT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"uvicorn\s+([A-Za-z_][A-Za-z0-9_.]*):([A-Za-z_][A-Za-z0-9_]*)")
        .expect("valid uvicorn script regex")
});

/// Check whether a distribution name appears in manifest dependencies.
pub(super) fn manifest_has_dependency(
    manifest: &crate::manifest::LoadedManifest,
    name: &str,
) -> bool {
    let needle = name.to_ascii_lowercase();
    manifest.dependencies.iter().any(|dep| dep.name == needle)
}

/// Build a reference origin for a config file.
pub(super) fn origin_for_file(
    root: &Path,
    path: &Path,
    label: impl Into<String>,
) -> ReferenceOrigin {
    ReferenceOrigin {
        file: rel_to_root(root, path),
        line: None,
        label: label.into(),
    }
}

/// [`origin_for_file`] for the TOML item `label` names in `path`, with the
/// line it is written on when the file reads and the label leads there.
pub(super) fn toml_origin(root: &Path, path: &Path, label: impl Into<String>) -> ReferenceOrigin {
    let mut origin = origin_for_file(root, path, label);
    origin.line = std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| crate::manifest::toml_label_line(&contents, &origin.label));
    origin
}

/// Collect partial-parse field names from list literal scans.
pub(super) fn partial_fields(scans: &BTreeMap<String, LiteralScan>) -> Vec<String> {
    scans
        .iter()
        .filter(|(_, scan)| !scan.complete)
        .map(|(field, _)| field.clone())
        .collect()
}

/// Find `settings.py` candidates under the project root (depth ≤ 4), outside
/// `skip_dirs`. A candidate must look like settings (#979): assign
/// `INSTALLED_APPS` or `ROOT_URLCONF` (in a django-configurations class too),
/// star-import the module that does, or `include(...)` it as
/// django-split-settings does. A `views/settings.py` does neither and is not one; a
/// file that cannot be read stays, so reading it reports the gap.
pub(super) fn find_settings_candidates(root: &Path, skip_dirs: &[&str]) -> Vec<String> {
    let mut candidates = Vec::new();
    collect_settings_files(root, root, 0, skip_dirs, &mut candidates);
    candidates.retain(|path| {
        read_to_string(&root.join(path))
            .map_or(true, |contents| SETTINGS_SHAPE_RE.is_match(&contents))
    });
    candidates.sort();
    candidates
}

#[expect(
    clippy::expect_used,
    reason = "the pattern is a literal; `Regex::new` fails only on a typo, which every run hits"
)]
static SETTINGS_SHAPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^[ \t]*(?:(?:INSTALLED_APPS|ROOT_URLCONF)\s*(?::[^=\n]*)?\+?=|from\s+\S+\s+import\s+\*|include\()",
    )
    .expect("valid settings shape regex")
});

fn collect_settings_files(
    root: &Path,
    current: &Path,
    depth: usize,
    skip_dirs: &[&str],
    out: &mut Vec<String>,
) {
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
                || skip_dirs.contains(&file_name.as_ref())
            {
                continue;
            }
            if !path_is_within_root(root, &path) {
                continue;
            }
            collect_settings_files(root, &path, depth + 1, skip_dirs, out);
        } else if file_name == "settings.py" && path_is_within_root(root, &path) {
            out.push(rel_to_root(root, &path));
        }
    }
}

/// Choose the best `settings.py` candidate when multiple exist.
pub(super) fn choose_settings_path(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_lexical_folds_dots_and_stays_under_root() {
        assert_eq!(
            join_lexical("pkg/db", "./migrations").as_deref(),
            Some("pkg/db/migrations")
        );
        assert_eq!(
            join_lexical("pkg/db", "..\\alembic").as_deref(),
            Some("pkg/alembic")
        );
        assert_eq!(join_lexical("pkg", ".").as_deref(), Some("pkg"));
        assert_eq!(join_lexical("", "../x"), None);
        assert_eq!(join_lexical("pkg", "/opt/app/etl.py"), None);
        assert_eq!(join_lexical("pkg", "\\\\host\\etl.py"), None);
        assert_eq!(join_lexical("", "C:\\proj\\etl.py"), None);
    }

    #[test]
    fn text_decorator_normalizes_like_parse_path() {
        let cases = [
            ("@app.route(\"/\")", Some(("app.route", true))),
            (
                "    @ app . route (\"/\")  # comment",
                Some(("app.route", true)),
            ),
            ("@app.route", Some(("app.route", false))),
            ("@apps[0].route(\"/\")", Some(("apps[].route", true))),
            ("@get_app().route(\"/\")", Some(("get_app.route", true))),
            ("@app.route(\"/:)\")", Some(("app.route", true))),
            ("@app.route(", Some(("app.route", true))),
            ("@app.task(bind=True)", Some(("app.task", true))),
            (
                "@functools.lru_cache(maxsize=cfg.get(\"n\"))",
                Some(("functools.lru_cache", true)),
            ),
            ("@register(app.task(x))", Some(("register", true))),
            ("@api.post(\"/\") if flag else f", None),
            ("@", None),
            ("x = \"@app.route('/')\"", None),
        ];
        for (line, expected) in cases {
            let actual = text_decorator(line);
            assert_eq!(
                actual
                    .as_ref()
                    .map(|(name, is_call)| (name.as_str(), *is_call)),
                expected,
                "{line}"
            );
        }
    }

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
    fn pytest_options_prefer_native_table_over_ini_options() {
        let label = |text: &str| {
            let table: toml::Table = toml::from_str(text).expect("toml");
            pytest_options_from_pyproject(&table).map(|(_, label)| label)
        };
        assert_eq!(
            label("[tool.pytest.ini_options]\ntestpaths = [\"t\"]\n"),
            Some("tool.pytest.ini_options")
        );
        assert_eq!(
            label("[tool.pytest]\ntestpaths = [\"t\"]\n"),
            Some("tool.pytest")
        );
        assert_eq!(
            label(
                "[tool.pytest]\nminversion = \"9.0\"\n[tool.pytest.ini_options]\ntestpaths = \"t\"\n"
            ),
            Some("tool.pytest")
        );
        assert_eq!(label("[tool.ruff]\n"), None);
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

    /// The search descends four directories below the root and skips the
    /// directories it is told to.
    #[test]
    fn settings_candidates_stop_at_depth_four_and_skip_dirs() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        for dir in ["a/b/c/d", "a/b/c/d/e", "examples/proj"] {
            let dir = temp.path().join(dir);
            std::fs::create_dir_all(&dir).expect("mkdir");
            std::fs::write(dir.join("settings.py"), "INSTALLED_APPS = []\n").expect("write");
        }
        assert_eq!(
            find_settings_candidates(temp.path(), &["examples"]),
            ["a/b/c/d/settings.py"]
        );
    }

    /// Run `check` on a context over `root`, whose inventory is `files` and
    /// whose workspace members are `members`.
    fn with_ctx(
        root: &Path,
        files: &[(&str, crate::sources::FileContext)],
        members: &[&str],
        check: impl FnOnce(&PluginContext<'_>),
    ) {
        use crate::discovery::{ProjectRoot, RootMarker};
        use crate::manifest::{LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata};
        use crate::sources::{
            DiscoveredFile, DiscoveredSources, FileKind, LayoutInfo, MemberLayout,
        };

        let root = ProjectRoot {
            path: root.to_path_buf(),
            marker: RootMarker::PyProjectToml,
        };
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                members: members
                    .iter()
                    .map(|path| MemberLayout {
                        path: (*path).to_owned(),
                        layout: LayoutInfo::default(),
                    })
                    .collect(),
                ..LayoutInfo::default()
            },
            effective_globs: Vec::new(),
            files: files
                .iter()
                .map(|(path, context)| DiscoveredFile {
                    path: (*path).to_owned(),
                    kind: FileKind::Python,
                    context: *context,
                })
                .collect(),
            warnings: Vec::new(),
        };
        let manifest = LoadedManifest {
            root: root.clone(),
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings::default(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        };
        let config = crate::config::default_config();
        let parse = crate::parser::ParseSummary::default();
        check(&PluginContext {
            root: &root,
            config: &config,
            sources: &sources,
            manifest: &manifest,
            parse: &parse,
        });
    }

    fn write_files(root: &Path, paths: &[&str]) {
        for path in paths {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, "").expect("write");
        }
    }

    #[test]
    fn project_dirs_are_the_root_then_each_member() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        with_ctx(temp.path(), &[], &["packages/etl"], |ctx| {
            assert_eq!(project_dirs(ctx).collect::<Vec<_>>(), ["", "packages/etl"]);
        });
    }

    /// An inventoried file keeps its own context, even one its path would
    /// not imply; any other file takes the context its path implies.
    #[test]
    fn file_context_prefers_the_inventory() {
        use crate::sources::FileContext;
        let temp = tempfile::TempDir::new().expect("tempdir");
        let files = [
            ("a/x.py", FileContext::Dev),
            ("flows/f.py", FileContext::Test),
        ];
        with_ctx(temp.path(), &files, &[], |ctx| {
            assert_eq!(file_context(ctx, "flows/f.py"), FileContext::Test);
            assert_eq!(file_context(ctx, "tests/t.py"), FileContext::Test);
            assert_eq!(file_context(ctx, "acme/m.py"), FileContext::Runtime);
        });
    }

    /// A module the inventory holds is answered from it, context included,
    /// without looking at the disk.
    #[test]
    fn module_paths_read_the_inventory_first() {
        use crate::sources::FileContext;
        let temp = tempfile::TempDir::new().expect("tempdir");
        let files = [
            ("acme/__init__.py", FileContext::Runtime),
            ("acme/jobs/a.py", FileContext::Test),
            ("acmex/b.py", FileContext::Runtime),
        ];
        with_ctx(temp.path(), &files, &[], |ctx| {
            assert_eq!(
                module_paths(ctx, "", "acme", true),
                [
                    ("acme/__init__.py".to_owned(), FileContext::Runtime),
                    ("acme/jobs/a.py".to_owned(), FileContext::Test),
                ]
            );
            assert_eq!(
                module_paths(ctx, "", "acme", false),
                [("acme/__init__.py".to_owned(), FileContext::Runtime)]
            );
        });
    }

    /// On disk, a directory without `__init__.py` is walked as a tree but
    /// names no module by itself.
    #[test]
    fn module_paths_on_disk_walk_a_namespace_only_as_a_tree() {
        use crate::sources::FileContext;
        let temp = tempfile::TempDir::new().expect("tempdir");
        write_files(temp.path(), &["ns/x.py", "src/pkg/__init__.py"]);
        with_ctx(temp.path(), &[], &[], |ctx| {
            assert_eq!(module_paths(ctx, "", "ns", false), []);
            assert_eq!(
                module_paths(ctx, "", "ns", true),
                [("ns/x.py".to_owned(), FileContext::Runtime)]
            );
            assert_eq!(
                module_paths(ctx, "", "pkg", false),
                [("src/pkg/__init__.py".to_owned(), FileContext::Runtime)]
            );
        });
    }

    /// The walk lists `.py` files only, skips hidden directories and
    /// `__pycache__`, and enters at most 16 directories below the start.
    #[test]
    fn walk_py_skips_hidden_cache_and_too_deep_directories() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let deep: Vec<String> = (1..=16).map(|n| format!("d{n}")).collect();
        let deep = deep.join("/");
        let ok = format!("w/{deep}/e.py");
        let too_deep = format!("w/{deep}/d17/f.py");
        write_files(
            temp.path(),
            &[
                "w/a.py",
                "w/notes.txt",
                "w/.hidden/b.py",
                "w/__pycache__/c.py",
                "w/sub/d.py",
                &ok,
                &too_deep,
            ],
        );
        let mut found = BTreeSet::new();
        walk_py(temp.path(), "w", &mut found);
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            ["w/a.py".to_owned(), ok, "w/sub/d.py".to_owned()]
        );
    }
}
