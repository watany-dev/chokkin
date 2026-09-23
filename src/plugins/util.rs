//! Shared helpers for plugin extractors.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::CharIndices;

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use toml::Value;

use crate::manifest::ManifestError;
use crate::manifest::literals::LiteralScan;
use crate::manifest::util::{
    path_is_within_root, read_to_string, relative_path as manifest_relative_path,
};
use crate::parser::{ParseSeverity, ParsedModule};
use crate::sources::{FileKind, path_to_module};

use super::context::PluginContext;
use super::error::PluginsError;
use super::types::{
    BinaryUsage, ModuleReference, PluginContribution, ReferenceOrigin, SymbolReference,
};

/// INI section key-value pairs.
pub type IniSection = BTreeMap<String, String>;

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
///
/// Uses the step 6 decorator sites when available and falls back to
/// [`text_decorator_line`] for standalone callers, with the same predicate on
/// both paths. Returns whether any reference was pushed.
pub fn push_decorated_modules(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    is_decorator: fn(&str, bool) -> bool,
    label: &str,
) -> bool {
    let mut found = false;
    if let Some(parse) = ctx.parse {
        for module in &parse.modules {
            let Some(line) = decorator_line(&ctx.root.path, module, is_decorator) else {
                continue;
            };
            found |= push_decorated_module(ctx, contrib, &module.path, line, label);
        }
        return found;
    }

    // Standalone callers run before step 6, so there is nothing to reuse.
    for file in &ctx.sources.files {
        if file.kind != FileKind::Python {
            continue;
        }
        let path = ctx.root.path.join(&file.path);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(line) = text_decorator_line(&contents, is_decorator) else {
            continue;
        };
        found |= push_decorated_module(ctx, contrib, &file.path, line, label);
    }
    found
}

fn push_decorated_module(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    file: &str,
    line: u32,
    label: &str,
) -> bool {
    let Some(module) = path_to_module(file, &ctx.sources.layout) else {
        return false;
    };
    contrib.module_refs.push(ModuleReference {
        module,
        origin: ReferenceOrigin {
            file: file.to_owned(),
            line: Some(line),
            label: label.to_owned(),
        },
    });
    true
}

/// Push a symbol reference when `value` parses as `module:symbol`.
pub fn push_symbol_ref(contrib: &mut PluginContribution, value: &str, origin: ReferenceOrigin) {
    if let Some((module, symbol)) = parse_module_symbol(value) {
        contrib.symbol_refs.push(SymbolReference {
            module,
            symbol,
            origin,
        });
    }
}

/// Record a CLI binary usage.
pub fn push_binary(contrib: &mut PluginContribution, binary: &str, origin: ReferenceOrigin) {
    contrib.binary_usages.push(BinaryUsage {
        binary: binary.to_owned(),
        origin,
    });
}

/// Count leading ASCII spaces (YAML indentation).
pub fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

/// Text fallback for [`decorator_line`] when no parse output is available.
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
pub fn decorator_suffix(name: &str) -> (Option<&str>, &str) {
    name.rsplit_once('.')
        .map_or((None, name), |(receiver, suffix)| (Some(receiver), suffix))
}

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
            if !path_is_within_root(root, &path) {
                continue;
            }
            collect_settings_files(root, &path, depth + 1, out);
        } else if file_name == "settings.py" && path_is_within_root(root, &path) {
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
