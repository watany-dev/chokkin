//! Parse one Python source file and orchestrate project-wide parsing.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use ruff_python_ast::Stmt;
use ruff_python_parser::ParseError as RuffParseError;
use serde_json::Value;

use crate::VERSION;
use crate::cache::{
    CacheKeyContext, CacheOptions, ParseCacheBundle, ParseCacheBundleRef, ParseCacheKey,
    SourceFingerprint, stable_list_hash,
};
use crate::config::TargetVersion;
use crate::discovery::ProjectRoot;
use crate::sources::{DiscoveredFile, DiscoveredSources, FileKind, LayoutInfo};

use super::encoding::decode_python_source;
use super::error::ParseError;
use super::ignores::extract_ignores;
use super::lines::LineIndex;
use super::types::{ParseDiagnostic, ParseSeverity, ParseSummary, ParsedModule};
use super::visit::ModuleVisitor;

/// Parse one `.py` file under `root` (static only; never executes Python).
///
/// Syntax errors are recorded in [`ParsedModule::diagnostics`]; the function still
/// returns `Ok` unless the file cannot be read. A file that cannot be decoded
/// comes back with [`ParsedModule::skipped`] set.
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
    let Some(source) = read_source(root, path)? else {
        return Ok(skipped_module(path));
    };
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

fn read_source(root: &ProjectRoot, path: &str) -> Result<Option<String>, ParseError> {
    let absolute = root.path.join(path);
    let bytes = std::fs::read(&absolute).map_err(|source| ParseError::Io {
        path: absolute,
        source,
    })?;
    Ok(decode_python_source(bytes))
}

fn skipped_module(path: &str) -> ParsedModule {
    ParsedModule {
        path: path.to_owned(),
        skipped: true,
        ..ParsedModule::default()
    }
}

fn parse_python_source(
    path: &str,
    source: &str,
    layout: &LayoutInfo,
    file_context: crate::sources::FileContext,
    target: &TargetVersion,
) -> ParsedModule {
    let lines = LineIndex::new(source);
    let mut parsed = match ruff_python_parser::parse_module(source) {
        Ok(module) => {
            let mut visitor = ModuleVisitor::new(path, layout, file_context, &lines);
            visitor.visit_module(module.suite());
            let mut parsed = visitor.into_parsed();
            note_unsupported_syntax(target, module.suite(), &mut parsed.diagnostics);
            parsed
        },
        Err(error) => {
            let mut parsed = ParsedModule {
                path: path.to_owned(),
                ..ParsedModule::default()
            };
            parsed
                .diagnostics
                .push(syntax_diagnostic(path, &lines, &error));
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
    let Some(source) = read_source(root, path)? else {
        return Ok(skipped_module(path));
    };
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
    disk_cache: Option<&CacheOptions>,
) -> Result<ParseSummary, ParseError> {
    let layout = &sources.layout;
    let context = provisional_parse_cache_context(sources, target);
    let disk_cache = disk_cache.filter(|options| options.enabled);
    // Racy mtimes are judged against the filesystem's own clock so a lagging
    // network mount cannot make a just-written source look settled. The local
    // clock is the fallback when the cache directory is read-only: a warm
    // bundle is still usable there, and failing the run over the probe would
    // be a regression.
    let clock = disk_cache.map(|cache| {
        cache
            .filesystem_now(&root.path)
            .unwrap_or_else(|_| SystemTime::now())
    });

    // The bundle is read once up front and written once at the end. Keeping
    // only the entries this run touched prunes results for sources that have
    // since changed or disappeared, so the file tracks the project instead of
    // growing with every edit.
    let mut stored = read_disk_parse_bundle(disk_cache, &root.path, &context)?;
    let stored_len = stored.entries.len();

    let pending = collect_pending(root, sources, &context, clock)?;

    let mut slots: Vec<Option<ParsedModule>> = vec![None; pending.len()];
    drain_cache(&pending, &mut stored, &mut slots);
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
        slots[index] = Some(parsed);
    }

    if disk_cache.is_some() {
        let retained = retained_entries(&pending, &slots);
        // Without a parse, every retained entry came out of the stored bundle,
        // so a smaller count means vanished sources to prune.
        if parsed_any || retained.entry_ids().count() != stored_len {
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

/// Fill `slots` from the on-disk bundle.
///
/// Hits are moved out of `stored` rather than cloned: the bundle is not used
/// afterwards.
fn drain_cache(
    pending: &[PendingFile<'_>],
    stored: &mut ParseCacheBundle,
    slots: &mut [Option<ParsedModule>],
) {
    for (slot, entry) in slots.iter_mut().zip(pending) {
        if let Some(key) = &entry.key {
            *slot = stored.take(key);
        }
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
/// The cache is drained by the caller, so a worker only ever parses.
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
        unit_version: "parse-v12".to_owned(),
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
    // `from_root_relative_stat_at` identifies an unchanged source by `(size,
    // mtime)` and only reads bytes when that is ambiguous. On a warm run the
    // whole project used to be read and hashed just to build lookup keys.
    SourceFingerprint::from_root_relative_stat_at(&root.path, path, now).map_err(|source| {
        ParseError::Io {
            path: root.path.join(path),
            source,
        }
    })
}

fn syntax_diagnostic(path: &str, lines: &LineIndex, error: &RuffParseError) -> ParseDiagnostic {
    // `ParseError`'s own Display appends a byte range; the line is reported separately.
    ParseDiagnostic {
        line: lines.line(error.location.start()),
        message: format!("syntax error in `{path}`: {}", error.error),
        severity: ParseSeverity::Error,
    }
}

fn note_unsupported_syntax(
    target: &TargetVersion,
    stmts: &[Stmt],
    diagnostics: &mut Vec<ParseDiagnostic>,
) {
    if target.minor() < 12 && stmts.iter().any(Stmt::is_type_alias_stmt) {
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
        }
    }

    fn empty_layout() -> LayoutInfo {
        LayoutInfo {
            layout: ProjectLayout::Unknown,
            package_root: String::new(),
            packages: Vec::new(),
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
            members: Vec::new(),
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
        };
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: empty_layout(),
            effective_globs: Vec::new(),
            files,
            warnings: Vec::new(),
        };

        let summary = parse_project_sources_with_cache(
            &root,
            &sources,
            &TargetVersion::default_py311(),
            None,
        )
        .expect("parse");

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
            // A directory named like a source cannot be read as a file.
            if index == 60 || index == 150 {
                fs::create_dir(temp.path().join(name)).expect("mkdir");
            } else {
                fs::write(temp.path().join(name), "import os\n").expect("write");
            }
        }
        let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
        let (root, sources) = python_sources(temp.path(), &borrowed);

        for _ in 0..10 {
            let error = parse_project_sources_with_cache(
                &root,
                &sources,
                &TargetVersion::default_py311(),
                None,
            )
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
            Some(&CacheOptions::disabled()),
        )
        .expect("parse");

        assert!(!temp.path().join(".chokkin").exists());
    }

    #[test]
    fn parse_worker_count_stays_sequential_for_small_projects() {
        assert_eq!(parse_worker_count(0), 1);
        assert_eq!(parse_worker_count(32), 1);
        assert!(parse_worker_count(10_000) >= 1);
    }
}
