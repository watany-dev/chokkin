//! Parse one Python source file and orchestrate project-wide parsing.

use rustpython_parser::ast;
use rustpython_parser::source_code::RandomLocator;
use rustpython_parser::{Parse, ParseError as RpParseError};
use serde_json::Value;

use crate::VERSION;
use crate::cache::{
    CacheKeyContext, CacheOptions, ParseCacheKey, ParseCacheStore, SourceFingerprint,
    stable_hex_hash,
};
use crate::config::TargetVersion;
use crate::discovery::ProjectRoot;
use crate::sources::{DiscoveredSources, FileKind, LayoutInfo};

use super::error::ParseError;
use super::ignores::extract_ignores;
use super::syntax::{SyntaxFeature, feature_requirement, supports_syntax};
use super::types::{ParseDiagnostic, ParseSeverity, ParseSummary, ParsedModule};
use super::visit::ModuleVisitor;

/// Parse one `.py` file under `root` (static only; never executes Python).
///
/// Syntax errors are recorded in [`ParsedModule::diagnostics`]; the function still
/// returns `Ok` unless the file cannot be read.
///
/// # Errors
///
/// Returns [`ParseError::Io`] when the file cannot be read.
pub fn parse_file(
    root: &ProjectRoot,
    path: &str,
    layout: &LayoutInfo,
    file_context: crate::sources::FileContext,
    target: &TargetVersion,
) -> Result<ParsedModule, ParseError> {
    let absolute = root.path.join(path);
    let source = std::fs::read_to_string(&absolute).map_err(|source| ParseError::Io {
        path: absolute,
        source,
    })?;
    Ok(parse_python_source(
        path,
        &source,
        layout,
        file_context,
        target,
    ))
}

fn parse_python_source(
    path: &str,
    source: &str,
    layout: &LayoutInfo,
    file_context: crate::sources::FileContext,
    target: &TargetVersion,
) -> ParsedModule {
    let mut locator = RandomLocator::new(source);
    let mut parsed = match ast::Suite::parse(source, path) {
        Ok(stmts) => {
            let mut visitor = ModuleVisitor::new(path, layout, file_context, &mut locator);
            visitor.visit_module(&stmts);
            let mut parsed = visitor.into_parsed();
            note_unsupported_syntax(target, &stmts, &mut parsed.diagnostics);
            parsed
        },
        Err(error) => {
            let mut parsed = ParsedModule::empty(path.to_owned());
            parsed
                .diagnostics
                .push(syntax_diagnostic(path, &mut locator, &error, target));
            parsed
        },
    };

    parsed.ignores = extract_ignores(source);
    parsed
}

fn parse_notebook_file(
    root: &ProjectRoot,
    path: &str,
    layout: &LayoutInfo,
    file_context: crate::sources::FileContext,
    target: &TargetVersion,
) -> Result<ParsedModule, ParseError> {
    let absolute = root.path.join(path);
    let source = std::fs::read_to_string(&absolute).map_err(|source| ParseError::Io {
        path: absolute,
        source,
    })?;
    let extracted = match notebook_python_source(&source) {
        Ok(source) => source,
        Err(message) => {
            let mut parsed = ParsedModule::empty(path.to_owned());
            parsed.diagnostics.push(ParseDiagnostic {
                line: 0,
                message,
                severity: ParseSeverity::Warning,
            });
            return Ok(parsed);
        },
    };
    Ok(parse_python_source(
        path,
        &extracted,
        layout,
        file_context,
        target,
    ))
}

fn notebook_python_source(source: &str) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(source).map_err(|error| format!("invalid notebook JSON: {error}"))?;
    let Some(cells) = value.get("cells").and_then(Value::as_array) else {
        return Err("invalid notebook JSON: missing cells array".to_owned());
    };
    let mut extracted = String::new();
    for cell in cells {
        if cell.get("cell_type").and_then(Value::as_str) != Some("code") {
            continue;
        }
        let Some(source) = cell.get("source") else {
            continue;
        };
        push_notebook_cell_source(source, &mut extracted);
        extracted.push('\n');
    }
    Ok(extracted)
}

fn push_notebook_cell_source(source: &Value, extracted: &mut String) {
    if let Some(text) = source.as_str() {
        extracted.push_str(text);
        if !text.ends_with('\n') {
            extracted.push('\n');
        }
        return;
    }
    let Some(lines) = source.as_array() else {
        return;
    };
    for line in lines {
        if let Some(text) = line.as_str() {
            extracted.push_str(text);
            if !text.ends_with('\n') {
                extracted.push('\n');
            }
        }
    }
}

/// Parse all `.py` files in `sources`.
///
/// IO failures abort the whole operation. Syntax errors are recorded per file.
///
/// # Errors
///
/// Returns [`ParseError::Io`] when a source file cannot be read.
pub fn parse_project_sources(
    root: &ProjectRoot,
    sources: &DiscoveredSources,
    target: &TargetVersion,
) -> Result<ParseSummary, ParseError> {
    parse_project_sources_with_cache(root, sources, target, None, None)
}

/// Parse all `.py` files in `sources`, optionally reusing parse results from cache.
///
/// IO failures abort the whole operation. Syntax errors are recorded per file.
///
/// # Errors
///
/// Returns [`ParseError::Io`] when a source file cannot be read.
pub fn parse_project_sources_with_cache(
    root: &ProjectRoot,
    sources: &DiscoveredSources,
    target: &TargetVersion,
    mut cache: Option<&mut ParseCacheStore>,
    disk_cache: Option<&CacheOptions>,
) -> Result<ParseSummary, ParseError> {
    let layout = &sources.layout;
    let mut summary = ParseSummary::empty();
    let context = provisional_parse_cache_context(sources, target);

    for file in &sources.files {
        if file.kind == FileKind::Stub {
            summary.skipped_count = summary.skipped_count.saturating_add(1);
            continue;
        }

        let use_cache = cache.is_some() || disk_cache.is_some();
        let parsed = if use_cache {
            let key = parse_cache_key(root, &file.path, &context)?;
            if let Some(parsed) = cache
                .as_deref_mut()
                .and_then(|cache_store| cache_store.get(&key))
            {
                parsed
            } else if let Some(parsed) = read_disk_parse_cache(disk_cache, &root.path, &key)? {
                if let Some(cache_store) = cache.as_deref_mut() {
                    cache_store.insert(key, parsed.clone());
                }
                parsed
            } else {
                let parsed = parse_discovered_file(root, file, layout, target)?;
                write_disk_parse_cache(disk_cache, &root.path, &key, &parsed)?;
                if let Some(cache_store) = cache.as_deref_mut() {
                    cache_store.insert(key, parsed.clone());
                }
                parsed
            }
        } else {
            parse_discovered_file(root, file, layout, target)?
        };
        let has_syntax_error = parsed
            .diagnostics
            .iter()
            .any(|diag| diag.severity == ParseSeverity::Error);
        if has_syntax_error {
            summary.error_count = summary.error_count.saturating_add(1);
        }
        summary.parsed_count = summary.parsed_count.saturating_add(1);
        summary.modules.push(parsed);
    }

    Ok(summary)
}

fn parse_discovered_file(
    root: &ProjectRoot,
    file: &crate::sources::DiscoveredFile,
    layout: &LayoutInfo,
    target: &TargetVersion,
) -> Result<ParsedModule, ParseError> {
    match file.kind {
        FileKind::Python => parse_file(root, &file.path, layout, file.context, target),
        FileKind::Notebook => parse_notebook_file(root, &file.path, layout, file.context, target),
        FileKind::Stub => Ok(ParsedModule::empty(file.path.clone())),
    }
}

fn read_disk_parse_cache(
    cache: Option<&CacheOptions>,
    project_root: &std::path::Path,
    key: &ParseCacheKey,
) -> Result<Option<ParsedModule>, ParseError> {
    let Some(cache) = cache else {
        return Ok(None);
    };
    cache
        .read_parse_entry(project_root, key)
        .map_err(|source| ParseError::Io {
            path: cache.parse_entry_path(project_root, key),
            source,
        })
}

fn write_disk_parse_cache(
    cache: Option<&CacheOptions>,
    project_root: &std::path::Path,
    key: &ParseCacheKey,
    parsed: &ParsedModule,
) -> Result<(), ParseError> {
    let Some(cache) = cache else {
        return Ok(());
    };
    cache
        .write_parse_entry(project_root, key, parsed)
        .map_err(|source| ParseError::Io {
            path: cache.parse_entry_path(project_root, key),
            source,
        })
}

fn provisional_parse_cache_context(
    sources: &DiscoveredSources,
    target: &TargetVersion,
) -> CacheKeyContext {
    CacheKeyContext {
        chokkin_version: VERSION.to_owned(),
        config_hash: stable_hex_hash(format!("{:?}", sources.effective_globs).as_bytes()),
        manifest_hash: stable_hex_hash(format!("{:?}", sources.layout).as_bytes()),
        target_version: target.as_str().to_owned(),
        unit_version: "parse-v1".to_owned(),
    }
}

fn parse_cache_key(
    root: &ProjectRoot,
    path: &str,
    context: &CacheKeyContext,
) -> Result<ParseCacheKey, ParseError> {
    let source = SourceFingerprint::from_root_relative(&root.path, path).map_err(|source| {
        ParseError::Io {
            path: root.path.join(path),
            source,
        }
    })?;
    Ok(ParseCacheKey {
        context: context.clone(),
        source,
    })
}

fn syntax_diagnostic(
    path: &str,
    locator: &mut RandomLocator<'_>,
    error: &RpParseError,
    target: &TargetVersion,
) -> ParseDiagnostic {
    let line = locator.locate(error.offset).row.get();
    let mut message = format!("syntax error in `{path}`: {error}");
    if let Some(hint) = syntax_target_hint(error, target) {
        use std::fmt::Write as _;
        let _ = write!(message, " (requires {hint})");
    }
    ParseDiagnostic {
        line,
        message,
        severity: ParseSeverity::Error,
    }
}

fn syntax_target_hint(error: &RpParseError, target: &TargetVersion) -> Option<&'static str> {
    let text = error.to_string();
    if text.contains("match") && !supports_syntax(target, SyntaxFeature::MatchStatement) {
        return Some(feature_requirement(SyntaxFeature::MatchStatement));
    }
    if text.contains("type") && !supports_syntax(target, SyntaxFeature::TypeAliasStatement) {
        return Some(feature_requirement(SyntaxFeature::TypeAliasStatement));
    }
    None
}

fn note_unsupported_syntax(
    target: &TargetVersion,
    stmts: &[ast::Stmt],
    diagnostics: &mut Vec<ParseDiagnostic>,
) {
    if !supports_syntax(target, SyntaxFeature::TypeAliasStatement)
        && stmts.iter().any(ast::Stmt::is_type_alias_stmt)
    {
        diagnostics.push(ParseDiagnostic {
            line: 0,
            message: format!(
                "file uses `type` aliases; set target_version to {}",
                feature_requirement(SyntaxFeature::TypeAliasStatement)
            ),
            severity: ParseSeverity::Warning,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::sources::{FileContext, LayoutInfo, ProjectLayout};

    fn write_temp_py(dir: &Path, name: &str, contents: &str) -> ProjectRoot {
        fs::write(dir.join(name), contents).expect("write");
        ProjectRoot {
            path: dir.to_path_buf(),
            marker: RootMarker::PyProjectToml,
            start: dir.to_path_buf(),
        }
    }

    fn empty_layout() -> LayoutInfo {
        LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        }
    }

    #[test]
    fn parses_simple_import() {
        let temp = TempDir::new().expect("tempdir");
        let root = write_temp_py(
            temp.path(),
            "sample.py",
            "import os\nfrom sys import version\n",
        );
        let parsed = parse_file(
            &root,
            "sample.py",
            &empty_layout(),
            FileContext::Runtime,
            &TargetVersion::default_py311(),
        )
        .expect("parse");
        assert_eq!(parsed.imports.len(), 2);
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn syntax_error_becomes_diagnostic() {
        let temp = TempDir::new().expect("tempdir");
        let root = write_temp_py(temp.path(), "broken.py", "def broken(:\n");
        let parsed = parse_file(
            &root,
            "broken.py",
            &empty_layout(),
            FileContext::Runtime,
            &TargetVersion::default_py311(),
        )
        .expect("parse");
        assert!(parsed.imports.is_empty());
        assert_eq!(parsed.diagnostics.len(), 1);
    }
}
