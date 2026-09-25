//! Parse one Python source file and orchestrate project-wide parsing.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use rustpython_parser::ast;
use rustpython_parser::source_code::RandomLocator;
use rustpython_parser::{Parse, ParseError as RpParseError};
use serde_json::Value;

use crate::VERSION;
use crate::cache::{
    CacheKeyContext, CacheOptions, ParseCacheBundle, ParseCacheBundleRef, ParseCacheKey,
    ParseCacheStore, SourceFingerprint, stable_list_hash,
};
use crate::config::TargetVersion;
use crate::discovery::ProjectRoot;
use crate::sources::{DiscoveredFile, DiscoveredSources, FileKind, LayoutInfo};

use super::error::ParseError;
use super::ignores::extract_ignores;
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
    // A PEP 723 script runs under its own `requires-python`, so the syntax
    // gate follows it. Reading it from the text keeps the parse cache valid:
    // the file fingerprint already covers the block.
    let script_target = crate::manifest::inline_script_target(&source);
    Ok(parse_python_source(
        path,
        &source,
        layout,
        file_context,
        script_target.as_ref().unwrap_or(target),
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
            let mut parsed = ParsedModule {
                path: path.to_owned(),
                ..ParsedModule::default()
            };
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
            let mut parsed = ParsedModule {
                path: path.to_owned(),
                ..ParsedModule::default()
            };
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
    let disk_cache = disk_cache.filter(|options| options.enabled);
    if let Some(cache_store) = cache.as_deref_mut() {
        cache_store.reserve(sources.files.len());
    }
    let clock = parse_cache_clock(cache.is_some(), disk_cache, &root.path);

    // The bundle is read once up front and written once at the end. Keeping
    // only the entries this run touched prunes results for sources that have
    // since changed or disappeared, so the file tracks the project instead of
    // growing with every edit.
    let mut stored = read_disk_parse_bundle(disk_cache, &root.path, &context)?;
    let stored_ids: Vec<String> = stored.entries.keys().cloned().collect();

    let pending = collect_pending(root, sources, &context, clock)?;

    let mut slots: Vec<Option<ParsedModule>> = vec![None; pending.len()];
    drain_caches(&pending, &mut stored, cache.as_deref_mut(), &mut slots);
    drop(stored);

    let outstanding: Vec<usize> = slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| slot.is_none().then_some(index))
        .collect();
    let job = ParseJob {
        root,
        layout,
        target,
        pending: &pending,
        outstanding: &outstanding,
    };
    let mut parsed_any = false;
    for (index, parsed) in run_parse_job(&job)? {
        parsed_any = true;
        if let Some(store) = cache.as_deref_mut()
            && let Some(key) = &pending[index].key
        {
            store.insert(key.clone(), parsed.clone());
        }
        slots[index] = Some(parsed);
    }

    if disk_cache.is_some() {
        let retained = retained_entries(&pending, &slots);
        // Compare entry ids rather than counts: a store carried over from an
        // earlier run can serve a different set of the same size.
        if parsed_any || retained.entry_ids().ne(stored_ids.iter()) {
            write_disk_parse_bundle(disk_cache, &root.path, &context, &retained)?;
        }
    }

    Ok(ParseSummary {
        modules: slots.into_iter().flatten().collect(),
    })
}

/// Borrow every keyed module of this run for writing back to disk.
fn retained_entries<'a>(
    pending: &[PendingFile<'_>],
    slots: &'a [Option<ParsedModule>],
) -> ParseCacheBundleRef<'a> {
    let mut retained = ParseCacheBundleRef::default();
    for (slot, entry) in slots.iter().zip(pending) {
        if let (Some(key), Some(parsed)) = (&entry.key, slot) {
            retained.insert(key, parsed);
        }
    }
    retained
}

/// The clock racy mtimes are judged against, or `None` when no cache is in use.
///
/// Prefers the filesystem's own clock so a lagging network mount cannot make a
/// just-written source look settled. The local clock is the fallback when there
/// is no cache directory to probe, or it is read-only: a warm bundle is still
/// usable there, and failing the run over the probe would be a regression.
fn parse_cache_clock(
    memory_cache: bool,
    disk_cache: Option<&CacheOptions>,
    project_root: &std::path::Path,
) -> Option<SystemTime> {
    if !memory_cache && disk_cache.is_none() {
        return None;
    }
    Some(
        disk_cache
            .and_then(|cache| cache.filesystem_now(project_root).ok())
            .unwrap_or_else(SystemTime::now),
    )
}

/// Collect the sources this run has to parse, each with its cache key.
///
/// Keys are only built when `clock` is set. Returns the pending files in
/// discovery order; stubs are skipped.
fn collect_pending<'a>(
    root: &ProjectRoot,
    sources: &'a DiscoveredSources,
    context: &CacheKeyContext,
    clock: Option<SystemTime>,
) -> Result<Vec<PendingFile<'a>>, ParseError> {
    let mut pending = Vec::with_capacity(sources.files.len());
    for file in &sources.files {
        if file.kind == FileKind::Stub {
            continue;
        }
        let key = clock
            .map(|now| parse_cache_key(root, &file.path, context, now))
            .transpose()?;
        pending.push(PendingFile { file, key });
    }
    Ok(pending)
}

/// Fill `slots` from the in-memory store and the on-disk bundle.
///
/// Both caches need `&mut` on the store, so they are drained here rather than
/// from the workers. Probing exactly once per file also keeps the hit/miss
/// counters identical to the sequential implementation. Disk hits are moved
/// out of `stored` rather than cloned: the bundle is not used afterwards.
fn drain_caches(
    pending: &[PendingFile<'_>],
    stored: &mut ParseCacheBundle,
    mut cache: Option<&mut ParseCacheStore>,
    slots: &mut [Option<ParsedModule>],
) {
    for (slot, entry) in slots.iter_mut().zip(pending) {
        let Some(key) = &entry.key else {
            continue;
        };
        *slot = match cache.as_deref_mut() {
            Some(store) => store.get_or_take(key, stored),
            None => stored.take(key),
        };
    }
}

struct PendingFile<'a> {
    file: &'a DiscoveredFile,
    key: Option<ParseCacheKey>,
}

struct ParseJob<'a> {
    root: &'a ProjectRoot,
    layout: &'a LayoutInfo,
    target: &'a TargetVersion,
    pending: &'a [PendingFile<'a>],
    outstanding: &'a [usize],
}

impl ParseJob<'_> {
    fn run_one(&self, index: usize) -> Result<ParsedModule, ParseError> {
        let entry = &self.pending[index];
        parse_discovered_file(self.root, entry.file, self.layout, self.target)
    }
}

/// Parse every outstanding file, spread over threads.
///
/// Both caches are drained by the caller, so a worker only ever parses.
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
            .map(|_| scope.spawn(|| run_worker(job, &cursor)))
            .collect();
        handles
            .into_iter()
            .map(std::thread::ScopedJoinHandle::join)
            .collect::<Vec<_>>()
    });

    let mut parsed = Vec::with_capacity(job.outstanding.len());
    let mut first_error: Option<(usize, ParseError)> = None;
    for batch in batches {
        // Keep the sequential behaviour of letting a parser panic unwind the
        // caller instead of turning it into a silent error.
        match batch {
            Ok(Ok(done)) => parsed.extend(done),
            Ok(Err(failure)) => first_error = Some(earlier_failure(first_error, failure)),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
    match first_error {
        Some((_, error)) => Err(error),
        None => Ok(parsed),
    }
}

/// What one worker parsed, or the slot index and error it stopped at.
type WorkerOutcome = Result<Vec<(usize, ParsedModule)>, (usize, ParseError)>;

fn run_worker(job: &ParseJob<'_>, cursor: &AtomicUsize) -> WorkerOutcome {
    let mut done = Vec::new();
    loop {
        let next = cursor.fetch_add(1, Ordering::Relaxed);
        let Some(&index) = job.outstanding.get(next) else {
            return Ok(done);
        };
        match job.run_one(index) {
            Ok(parsed) => done.push((index, parsed)),
            Err(error) => return Err((index, error)),
        }
    }
}

/// Keep whichever failure comes first in discovery order.
///
/// Workers claim files in discovery order and only stop at their own failure,
/// so the earliest failing file has always been attempted: reporting it
/// matches the sequential path whatever the scheduling.
fn earlier_failure(
    current: Option<(usize, ParseError)>,
    candidate: (usize, ParseError),
) -> (usize, ParseError) {
    match current {
        Some(current) if current.0 < candidate.0 => current,
        _ => candidate,
    }
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
        FileKind::Stub => Ok(ParsedModule {
            path: file.path.clone(),
            ..ParsedModule::default()
        }),
    }
}

fn read_disk_parse_bundle(
    cache: Option<&CacheOptions>,
    project_root: &std::path::Path,
    context: &CacheKeyContext,
) -> Result<ParseCacheBundle, ParseError> {
    let Some(cache) = cache else {
        return Ok(ParseCacheBundle::default());
    };
    cache
        .read_parse_bundle(project_root, context)
        .map_err(|source| ParseError::Io {
            path: cache.parse_bundle_path(project_root, context),
            source,
        })
}

fn write_disk_parse_bundle(
    cache: Option<&CacheOptions>,
    project_root: &std::path::Path,
    context: &CacheKeyContext,
    bundle: &ParseCacheBundleRef<'_>,
) -> Result<(), ParseError> {
    let Some(cache) = cache else {
        return Ok(());
    };
    cache
        .write_parse_bundle_ref(project_root, context, bundle)
        .map_err(|source| ParseError::Io {
            path: cache.parse_bundle_path(project_root, context),
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
        unit_version: "parse-v6".to_owned(),
    }
}

fn parse_cache_key(
    root: &ProjectRoot,
    path: &str,
    context: &CacheKeyContext,
    now: SystemTime,
) -> Result<ParseCacheKey, ParseError> {
    // Parallel workers each need an owned key, so the shared context is cloned
    // per file here rather than mutated in place across a sequential loop.
    Ok(ParseCacheKey {
        context: context.clone(),
        source: source_fingerprint(root, path, now)?,
    })
}

fn source_fingerprint(
    root: &ProjectRoot,
    path: &str,
    now: SystemTime,
) -> Result<SourceFingerprint, ParseError> {
    // `from_root_relative_stat` identifies an unchanged source by `(size,
    // mtime)` and only reads bytes when that is ambiguous. On a warm run the
    // whole project used to be read and hashed just to build lookup keys.
    SourceFingerprint::from_root_relative_stat_at(&root.path, path, now).map_err(|source| {
        ParseError::Io {
            path: root.path.join(path),
            source,
        }
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
    if text.contains("match") && target.minor() < 10 {
        return Some("py310");
    }
    if text.contains("type") && target.minor() < 12 {
        return Some("py312");
    }
    None
}

fn note_unsupported_syntax(
    target: &TargetVersion,
    stmts: &[ast::Stmt],
    diagnostics: &mut Vec<ParseDiagnostic>,
) {
    if target.minor() < 12 && stmts.iter().any(ast::Stmt::is_type_alias_stmt) {
        diagnostics.push(ParseDiagnostic {
            line: 0,
            message: "file uses `type` aliases; set target_version to py312".to_owned(),
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
            .map(|site| (site.name.as_str(), site.line, site.is_call))
            .collect();
        assert_eq!(
            sites,
            vec![("shared_task", 4, false), ("app.route", 12, true)]
        );
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

        assert_eq!(summary.modules.len(), count);
        let actual: Vec<String> = summary
            .modules
            .iter()
            .map(|module| module.path.clone())
            .collect();
        assert_eq!(actual, expected);
    }

    fn python_sources(dir: &Path, names: &[&str]) -> (ProjectRoot, DiscoveredSources) {
        let root = ProjectRoot {
            path: dir.to_path_buf(),
            marker: RootMarker::PyProjectToml,
            start: dir.to_path_buf(),
        };
        let files = names
            .iter()
            .map(|name| DiscoveredFile {
                path: (*name).to_owned(),
                kind: FileKind::Python,
                context: FileContext::Runtime,
            })
            .collect();
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: empty_layout(),
            effective_globs: Vec::new(),
            files,
            warnings: Vec::new(),
        };
        (root, sources)
    }

    #[test]
    fn parallel_parse_reports_the_first_unreadable_file_in_discovery_order() {
        let temp = TempDir::new().expect("tempdir");
        let names: Vec<String> = (0..200).map(|index| format!("mod_{index:04}.py")).collect();
        for (index, name) in names.iter().enumerate() {
            let contents: &[u8] = if index == 60 || index == 150 {
                b"\xff\xfe not utf-8\n"
            } else {
                b"import os\n"
            };
            fs::write(temp.path().join(name), contents).expect("write");
        }
        let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
        let (root, sources) = python_sources(temp.path(), &borrowed);

        for _ in 0..10 {
            let error = parse_project_sources(&root, &sources, &TargetVersion::default_py311())
                .expect_err("unreadable sources must fail the parse");
            let ParseError::Io { path, .. } = error;
            assert!(path.ends_with("mod_0060.py"), "reported {}", path.display());
        }
    }

    #[test]
    fn disabled_disk_cache_touches_no_cache_directory() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("app.py"), "import os\n").expect("write");
        let (root, sources) = python_sources(temp.path(), &["app.py"]);

        parse_project_sources_with_cache(
            &root,
            &sources,
            &TargetVersion::default_py311(),
            None,
            Some(&CacheOptions::disabled()),
        )
        .expect("parse");

        assert!(!temp.path().join(".chokkin").exists());
    }

    #[test]
    fn bundle_is_rewritten_when_a_carried_store_serves_a_different_set() {
        let temp = TempDir::new().expect("tempdir");
        // Settled mtimes keep the keys stat-only, so they stay equal across
        // runs however slowly the test executes.
        let settled = SystemTime::now() - std::time::Duration::from_secs(60);
        for name in ["aaa.py", "bbb.py"] {
            let path = temp.path().join(name);
            fs::write(&path, "import os\n").expect("write");
            fs::File::options()
                .write(true)
                .open(&path)
                .expect("open")
                .set_times(fs::FileTimes::new().set_modified(settled))
                .expect("backdate mtime");
        }
        let target = TargetVersion::default_py311();
        let disk = CacheOptions::default();
        let mut store = ParseCacheStore::new();
        let mut run = |names: &[&str]| {
            let (root, sources) = python_sources(temp.path(), names);
            parse_project_sources_with_cache(
                &root,
                &sources,
                &target,
                Some(&mut store),
                Some(&disk),
            )
            .expect("parse");
            let context = provisional_parse_cache_context(&sources, &target);
            fs::read_to_string(disk.parse_bundle_path(temp.path(), &context)).expect("bundle")
        };

        run(&["aaa.py", "bbb.py"]);
        run(&["aaa.py"]);
        // Same entry count as the bundle on disk, but served from memory
        // without parsing: only a content check notices the swap.
        let bundle = run(&["bbb.py"]);

        assert!(
            bundle.contains("bbb.py"),
            "bundle kept the stale set: {bundle}"
        );
        assert!(!bundle.contains("aaa.py"));
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
