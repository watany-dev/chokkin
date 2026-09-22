//! Parse one Python source file and orchestrate project-wide parsing.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustpython_parser::ast;
use rustpython_parser::source_code::RandomLocator;
use rustpython_parser::{Parse, ParseError as RpParseError};
use serde_json::Value;

use crate::VERSION;
use crate::cache::{
    CacheKeyContext, CacheOptions, ParseCacheKey, ParseCacheStore, SourceFingerprint,
    stable_list_hash,
};
use crate::config::TargetVersion;
use crate::discovery::ProjectRoot;
use crate::sources::{DiscoveredFile, DiscoveredSources, FileKind, LayoutInfo};

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
    let context = provisional_parse_cache_context(sources, target);
    let use_cache = cache.is_some() || disk_cache.is_some();
    if let Some(cache_store) = cache.as_deref_mut() {
        cache_store.reserve(sources.files.len());
    }

    let mut summary = ParseSummary::empty();
    let mut pending: Vec<PendingFile<'_>> = Vec::with_capacity(sources.files.len());
    for file in &sources.files {
        if file.kind == FileKind::Stub {
            summary.skipped_count = summary.skipped_count.saturating_add(1);
            continue;
        }
        let key = if use_cache {
            Some(parse_cache_key(root, &file.path, &context)?)
        } else {
            None
        };
        pending.push(PendingFile { file, key });
    }

    // `ParseCacheStore` needs `&mut`, so it is drained here rather than from the
    // workers. Probing exactly once per file also keeps its hit/miss counters
    // identical to the sequential implementation.
    let mut slots: Vec<Option<ParsedModule>> = vec![None; pending.len()];
    if let Some(store) = cache.as_deref_mut() {
        for (slot, entry) in slots.iter_mut().zip(&pending) {
            if let Some(key) = &entry.key {
                *slot = store.get(key);
            }
        }
    }

    let outstanding: Vec<usize> = slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| slot.is_none().then_some(index))
        .collect();
    let job = ParseJob {
        root,
        layout,
        target,
        disk_cache,
        pending: &pending,
        outstanding: &outstanding,
    };
    for (index, parsed) in run_parse_job(&job)? {
        if let Some(store) = cache.as_deref_mut()
            && let Some(key) = &pending[index].key
        {
            store.insert(key.clone(), parsed.clone());
        }
        slots[index] = Some(parsed);
    }

    for parsed in slots.into_iter().flatten() {
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

struct PendingFile<'a> {
    file: &'a DiscoveredFile,
    key: Option<ParseCacheKey>,
}

struct ParseJob<'a> {
    root: &'a ProjectRoot,
    layout: &'a LayoutInfo,
    target: &'a TargetVersion,
    disk_cache: Option<&'a CacheOptions>,
    pending: &'a [PendingFile<'a>],
    outstanding: &'a [usize],
}

impl ParseJob<'_> {
    fn run_one(&self, index: usize) -> Result<ParsedModule, ParseError> {
        let entry = &self.pending[index];
        if let Some(key) = &entry.key
            && let Some(parsed) = read_disk_parse_cache(self.disk_cache, &self.root.path, key)?
        {
            return Ok(parsed);
        }
        let parsed = parse_discovered_file(self.root, entry.file, self.layout, self.target)?;
        if let Some(key) = &entry.key {
            write_disk_parse_cache(self.disk_cache, &self.root.path, key, &parsed)?;
        }
        Ok(parsed)
    }
}

/// Parse (or load from disk cache) every outstanding file, spread over threads.
///
/// Results carry their slot index because workers finish out of order; the
/// caller reassembles them in discovery order.
fn run_parse_job(job: &ParseJob<'_>) -> Result<Vec<(usize, ParsedModule)>, ParseError> {
    let workers = parse_worker_count(job.outstanding.len());
    if workers <= 1 {
        return job
            .outstanding
            .iter()
            .map(|&index| job.run_one(index).map(|parsed| (index, parsed)))
            .collect();
    }

    // A shared cursor rather than fixed chunks: file sizes vary by an order of
    // magnitude, so static splits leave workers idle behind one large module.
    let cursor = AtomicUsize::new(0);
    let batches = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let next = cursor.fetch_add(1, Ordering::Relaxed);
                        let Some(&index) = job.outstanding.get(next) else {
                            return Ok(done);
                        };
                        done.push((index, job.run_one(index)?));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(std::thread::ScopedJoinHandle::join)
            .collect::<Vec<_>>()
    });

    let mut parsed = Vec::with_capacity(job.outstanding.len());
    for batch in batches {
        // Keep the sequential behaviour of letting a parser panic unwind the
        // caller instead of turning it into a silent error.
        match batch {
            Ok(done) => parsed.extend(done?),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
    Ok(parsed)
}

fn parse_worker_count(files: usize) -> usize {
    // Below this, thread setup costs more than the parsing it overlaps.
    const MIN_FILES_PER_WORKER: usize = 32;
    if files <= MIN_FILES_PER_WORKER {
        return 1;
    }
    let available = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    available.min(files.div_ceil(MIN_FILES_PER_WORKER)).max(1)
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
        config_hash: stable_list_hash(&sources.effective_globs),
        manifest_hash: sources.layout.cache_key_hash(),
        target_version: target.as_str().to_owned(),
        unit_version: "parse-v3".to_owned(),
    }
}

fn parse_cache_key(
    root: &ProjectRoot,
    path: &str,
    context: &CacheKeyContext,
) -> Result<ParseCacheKey, ParseError> {
    // Parallel workers each need an owned key, so the shared context is cloned
    // per file here rather than mutated in place across a sequential loop.
    Ok(ParseCacheKey {
        context: context.clone(),
        source: source_fingerprint(root, path)?,
    })
}

fn source_fingerprint(root: &ProjectRoot, path: &str) -> Result<SourceFingerprint, ParseError> {
    // `from_root_relative_stat` identifies an unchanged source by `(size,
    // mtime)` and only reads bytes when that is ambiguous. On a warm run the
    // whole project used to be read and hashed just to build lookup keys.
    SourceFingerprint::from_root_relative_stat(&root.path, path).map_err(|source| ParseError::Io {
        path: root.path.join(path),
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
    fn records_decorator_sites_including_nested_definitions() {
        let temp = TempDir::new().expect("tempdir");
        let root = write_temp_py(
            temp.path(),
            "app.py",
            "from flask import Flask\n\n\n@shared_task\ndef refresh():\n    return None\n\n\ndef create_app():\n    app = Flask(__name__)\n\n    @app.route(\"/\")\n    def index():\n        return \"ok\"\n\n    return app\n",
        );
        let parsed = parse_file(
            &root,
            "app.py",
            &empty_layout(),
            FileContext::Runtime,
            &TargetVersion::default_py311(),
        )
        .expect("parse");
        let sites: Vec<_> = parsed
            .decorator_sites
            .iter()
            .map(|site| (site.name.as_str(), site.line))
            .collect();
        assert_eq!(sites, vec![("shared_task", 4), ("app.route", 12)]);
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
    fn parallel_parse_preserves_discovery_order() {
        // Enough files to cross `parse_worker_count`'s threshold, so this walks
        // the threaded path while the assertion pins module order to discovery
        // order regardless of which worker finished first.
        let temp = TempDir::new().expect("tempdir");
        let count = 200;
        let mut files = Vec::with_capacity(count);
        for index in 0..count {
            let name = format!("mod_{index:04}.py");
            fs::write(
                temp.path().join(&name),
                format!("import os\nVALUE_{index} = {index}\n"),
            )
            .expect("write");
            files.push(DiscoveredFile {
                path: name,
                kind: FileKind::Python,
                context: FileContext::Runtime,
            });
        }
        let expected: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
        let root = ProjectRoot {
            path: temp.path().to_path_buf(),
            marker: RootMarker::PyProjectToml,
            start: temp.path().to_path_buf(),
        };
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: empty_layout(),
            effective_globs: Vec::new(),
            files,
            warnings: Vec::new(),
        };

        let summary =
            parse_project_sources(&root, &sources, &TargetVersion::default_py311()).expect("parse");

        assert_eq!(summary.parsed_count, u32::try_from(count).expect("count"));
        let actual: Vec<String> = summary
            .modules
            .iter()
            .map(|module| module.path.clone())
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn parse_worker_count_stays_sequential_for_small_projects() {
        assert_eq!(parse_worker_count(0), 1);
        assert_eq!(parse_worker_count(32), 1);
        assert!(parse_worker_count(10_000) >= 1);
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
