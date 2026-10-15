//! Directory walking with glob, exclude, and gitignore filters.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use globset::GlobSet;
use ignore::overrides::Override;
use ignore::{DirEntry, WalkBuilder};

use crate::config::EntrySpec;
use crate::path_util::normalize_rel_path;

use super::cache::{Ancestors, DirIgnore, RootLoad, SourcesCache, gitignore_skips};
use super::context::assign_file_context;
use super::error::SourcesError;
use super::types::{DiscoveredFile, FileContext, FileKind};
use super::warnings::SourcesWarning;

const LARGE_PROJECT_THRESHOLD: usize = 10_000;
const GITIGNORE_PATH: &str = ".gitignore";

/// Options for [`collect_files`].
pub(super) struct CollectOptions<'a> {
    /// Project root directory.
    pub root: &'a Path,
    /// Globs selecting project files.
    pub project_matcher: &'a GlobSet,
    /// Globs excluding files from analysis.
    pub exclude_matcher: &'a Arc<GlobSet>,
    /// pytest `testpaths` files, test context whatever their path says.
    pub test_matcher: &'a GlobSet,
    /// Raw exclude patterns (for directory pruning via overrides).
    pub exclude_patterns: &'a [String],
    /// Whether to respect `.gitignore` files during the walk.
    pub respect_gitignore: bool,
    /// Optional root `.gitignore` for post-filtering.
    pub gitignore: Option<&'a DirIgnore>,
    /// Whether to drop non-runtime contexts.
    pub production: bool,
    /// Ignore files and globs parsed by earlier walks.
    pub cache: &'a SourcesCache,
}

/// What [`collect_files`] found under its root.
#[derive(Debug, Default)]
pub(super) struct Collected {
    /// The inventory, sorted by path.
    pub files: Vec<DiscoveredFile>,
    /// Under `production`, the test files left out of `files`, sorted by
    /// path: a library's tests still show which public names are called
    /// (#588), and keeping them spares a second walk (#844).
    pub held_tests: Vec<DiscoveredFile>,
    /// What the walk could not read.
    pub warnings: Vec<SourcesWarning>,
}

/// Load `.gitignore` from the project root when present.
pub(super) fn load_gitignore(
    root: &Path,
    cache: &SourcesCache,
) -> (Option<Arc<DirIgnore>>, Option<SourcesWarning>) {
    let dir = cache.dir(root);
    match dir.root_load() {
        RootLoad::Loaded => (Some(dir), None),
        RootLoad::Missing => (None, None),
        RootLoad::Unreadable => (
            None,
            Some(SourcesWarning::GitignoreUnreadable {
                path: GITIGNORE_PATH.to_owned(),
            }),
        ),
    }
}

pub(super) fn should_prune_dir(rel_str: &str, exclude_matcher: &GlobSet) -> bool {
    if rel_str.is_empty() {
        return false;
    }
    if exclude_matcher.is_match(rel_str) {
        return true;
    }
    exclude_matcher.is_match(format!("{rel_str}/**"))
}

/// Gitignore state of one walk: its root's ancestors and the walked
/// directories from the root down to the entry being filtered.
struct GitignoreFilter {
    cache: SourcesCache,
    root: PathBuf,
    ancestors: Option<Arc<Ancestors>>,
    /// Paths and ignore files of the current directory chain, root first.
    stack: Mutex<(Vec<PathBuf>, Vec<Arc<DirIgnore>>)>,
}

impl GitignoreFilter {
    fn new(root: &Path, cache: &SourcesCache, ancestors: Option<Arc<Ancestors>>) -> Self {
        Self {
            cache: cache.clone(),
            root: root.to_path_buf(),
            ancestors,
            stack: Mutex::new((Vec::new(), Vec::new())),
        }
    }

    fn skips(&self, path: &Path, is_dir: bool) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        let mut guard = self.stack.lock().unwrap_or_else(PoisonError::into_inner);
        let (paths, dirs) = &mut *guard;
        // Most entries sit in the directory of the previous one; skip the
        // component-wise comparisons below for them.
        if paths
            .last()
            .is_some_and(|top| top.as_os_str() == parent.as_os_str())
        {
            let skips = gitignore_skips(dirs, self.ancestors.as_deref(), &self.root, path, is_dir);
            drop(guard);
            return skips;
        }
        // The walk is depth-first: drop the directories it has left, then
        // add the ones it entered since the last entry.
        while paths.last().is_some_and(|top| !parent.starts_with(top)) {
            paths.pop();
            dirs.pop();
        }
        if paths.is_empty() {
            paths.push(self.root.clone());
            dirs.push(self.cache.dir(&self.root));
        }
        let Some(top) = paths.last() else {
            return false;
        };
        let Ok(below) = parent.strip_prefix(top) else {
            return false;
        };
        let mut current = top.clone();
        for component in below.components() {
            current.push(component);
            dirs.push(self.cache.dir(&current));
            paths.push(current.clone());
        }
        let skips = gitignore_skips(dirs, self.ancestors.as_deref(), &self.root, path, is_dir);
        drop(guard);
        skips
    }
}

/// The filters a walk applies to each entry below its root, the checks
/// `filter_entry` makes, so a replay can make them on recorded entries.
struct EntryFilter {
    root: PathBuf,
    overrides: Arc<Override>,
    exclude_matcher: Arc<GlobSet>,
    gitignore: Option<GitignoreFilter>,
}

impl EntryFilter {
    /// Whether a walk from `root` would yield `path`, given that it yields
    /// the parent. A path outside the root is not this filter's to judge.
    fn admits(&self, path: &Path, is_dir: bool) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return true;
        };
        if self.overrides.matched(rel, is_dir).is_ignore() {
            return false;
        }
        if self
            .gitignore
            .as_ref()
            .is_some_and(|filter| filter.skips(path, is_dir))
        {
            return false;
        }
        if is_dir {
            let rel_str = normalize_rel_path(rel);
            return !should_prune_dir(&rel_str, &self.exclude_matcher);
        }
        true
    }
}

/// A walker from `start` that yields what `filter` admits and tells
/// `recorder` about each entry it turns away.
fn configure_walker(
    start: &Path,
    filter: Arc<EntryFilter>,
    recorder: Option<Recorder>,
) -> WalkBuilder {
    let mut builder = WalkBuilder::new(start);
    // The walker reads no ignore files itself: `filter_entry` answers from
    // the shared cache what `git_ignore`/`git_exclude` would (#757).
    builder
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .follow_links(false)
        .hidden(false)
        .filter_entry(move |entry| {
            let kind = entry_kind(entry);
            if filter.admits(entry.path(), kind == Some(EntryKind::Dir)) {
                return true;
            }
            if let (Some(recorder), Some(kind)) = (&recorder, kind) {
                recorder.entry(entry.path(), kind, false);
            }
            false
        });
    builder
}

/// What a walked entry is to the collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Dir,
    File,
    Other,
}

fn entry_kind(entry: &DirEntry) -> Option<EntryKind> {
    let file_type = entry.file_type()?;
    // A symlinked root keeps its link type but is walked as a directory.
    Some(
        if file_type.is_dir() || (entry.depth() == 0 && entry.path().is_dir()) {
            EntryKind::Dir
        } else if file_type.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        },
    )
}

/// Ignore file errors of a walked directory, as `ignore` attaches them to
/// its entry.
fn dir_warning(cache: &SourcesCache, path: &Path) -> Option<SourcesWarning> {
    let reason = cache.dir(path).error()?;
    Some(SourcesWarning::PathUnreadable {
        path: path.to_string_lossy().into_owned(),
        reason,
    })
}

/// A walk error as a warning on the path that failed, so the root walk and a
/// member walk that meet the same directory say the same thing (#900). A
/// member root joins `packages/core` onto the root, so the path is rebuilt
/// from its components to spell it with the platform's separator either way.
/// Anything else names the walk's root, as all of them did before: an error
/// without a path, and an ignore file that does not parse, whose text already
/// names the file (only the reference walker, which reads ignore files through
/// `ignore`, meets that one).
pub(super) fn walk_error_warning(root: &Path, error: &ignore::Error) -> SourcesWarning {
    let (path, reason) = failed_path(error).unwrap_or((root, error));
    SourcesWarning::PathUnreadable {
        path: path
            .components()
            .collect::<PathBuf>()
            .to_string_lossy()
            .into_owned(),
        reason: reason.to_string(),
    }
}

/// The path an `ignore` walk error could not read and the I/O error under
/// it, whose text leaves the path out. The depth wrapper differs between
/// walks and is dropped.
fn failed_path(error: &ignore::Error) -> Option<(&Path, &ignore::Error)> {
    match error {
        ignore::Error::WithPath { path, err } if matches!(**err, ignore::Error::Io(_)) => {
            Some((path, err))
        },
        ignore::Error::WithDepth { err, .. } => failed_path(err),
        _ => None,
    }
}

/// The source kind `path`'s extension names, or `None` for a file the
/// walk does not collect.
fn source_kind(path: &Path) -> Option<FileKind> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("py") => Some(FileKind::Python),
        Some(ext) if ext.eq_ignore_ascii_case("pyi") => Some(FileKind::Stub),
        Some(ext) if ext.eq_ignore_ascii_case("ipynb") => Some(FileKind::Notebook),
        _ => None,
    }
}

/// Gathers what one walk yields into a [`Collected`].
struct Collector<'o, 'a> {
    options: &'o CollectOptions<'a>,
    collected: Collected,
}

impl<'o, 'a> Collector<'o, 'a> {
    fn new(options: &'o CollectOptions<'a>) -> Self {
        Self {
            options,
            collected: Collected::default(),
        }
    }

    /// The filter a walk from the root applies; reading the root's
    /// ancestors may warn first.
    fn filter(&mut self) -> Result<Arc<EntryFilter>, SourcesError> {
        let options = self.options;
        let gitignore = options.respect_gitignore.then(|| {
            let ancestors = options.cache.ancestors(options.root).map(Arc::new);
            if let Some(reason) = ancestors.as_deref().and_then(Ancestors::error) {
                self.collected
                    .warnings
                    .push(SourcesWarning::PathUnreadable {
                        path: options.root.to_string_lossy().into_owned(),
                        reason,
                    });
            }
            GitignoreFilter::new(options.root, options.cache, ancestors)
        });
        Ok(Arc::new(EntryFilter {
            root: options.root.to_path_buf(),
            overrides: options.cache.exclude_overrides(options.exclude_patterns)?,
            exclude_matcher: Arc::clone(options.exclude_matcher),
            gitignore,
        }))
    }

    fn visit(&mut self, path: &Path, kind: EntryKind) {
        match kind {
            EntryKind::Dir if self.options.respect_gitignore => {
                let warning = dir_warning(self.options.cache, path);
                self.collected.warnings.extend(warning);
            },
            EntryKind::File => self.visit_file(path),
            EntryKind::Dir | EntryKind::Other => {},
        }
    }

    fn visit_file(&mut self, path: &Path) {
        let options = self.options;
        let Some(kind) = source_kind(path) else {
            return;
        };
        let Ok(rel) = path.strip_prefix(options.root) else {
            return;
        };
        let rel_str = normalize_rel_path(rel);
        if !options.project_matcher.is_match(&rel_str) || options.exclude_matcher.is_match(&rel_str)
        {
            return;
        }
        if options
            .gitignore
            .is_some_and(|gitignore| gitignore.gitignore_ignores_file(rel))
        {
            return;
        }
        let context = if options.test_matcher.is_match(&rel_str) {
            FileContext::Test
        } else {
            assign_file_context(&rel_str)
        };
        let file = DiscoveredFile {
            path: rel_str,
            kind,
            context,
        };
        if options.production && !context.is_included_in_production() {
            if context == FileContext::Test {
                self.collected.held_tests.push(file);
            }
            return;
        }
        self.collected.files.push(file);
    }

    fn finish(mut self) -> Collected {
        let collected = &mut self.collected;
        collected
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
        collected
            .held_tests
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.collected
    }
}

/// Walk from `start` into `collector`, telling `recorder` what the walk met.
fn walk_into(
    start: &Path,
    filter: &Arc<EntryFilter>,
    collector: &mut Collector<'_, '_>,
    recorder: Option<&Recorder>,
) {
    for result in configure_walker(start, Arc::clone(filter), recorder.cloned()).build() {
        match result {
            Ok(entry) => {
                let Some(kind) = entry_kind(&entry) else {
                    continue;
                };
                if let Some(recorder) = recorder {
                    recorder.entry(entry.path(), kind, true);
                }
                collector.visit(entry.path(), kind);
            },
            Err(error) => {
                let warning = walk_error_warning(collector.options.root, &error);
                if let Some(recorder) = recorder {
                    recorder.push(Event::Failed {
                        path: failed_path(&error).map(|(path, _)| path.to_path_buf()),
                        warning: warning.clone(),
                    });
                }
                collector.collected.warnings.push(warning);
            },
        }
    }
}

/// Collect Python-related files under `root`.
pub(super) fn collect_files(options: &CollectOptions<'_>) -> Result<Collected, SourcesError> {
    let mut collector = Collector::new(options);
    let filter = collector.filter()?;
    walk_into(options.root, &filter, &mut collector, None);
    Ok(collector.finish())
}

/// [`collect_files`], keeping what the walk met so that walks from
/// directories below the root can be replayed from it (#843).
pub(super) fn record_files(
    options: &CollectOptions<'_>,
) -> Result<(Collected, WalkRecord), SourcesError> {
    let recorder = Recorder::default();
    let mut collector = Collector::new(options);
    let filter = collector.filter()?;
    walk_into(options.root, &filter, &mut collector, Some(&recorder));
    let events = std::mem::take(&mut *recorder.0.lock().unwrap_or_else(PoisonError::into_inner));
    Ok((collector.finish(), WalkRecord::new(options.root, events)))
}

/// [`collect_files`] for a root below `record`'s, answered from the record
/// instead of reading the tree again (#843).
///
/// The record holds every directory and source file the earlier walk met,
/// including the ones its filters turned away, in walk order. Each is put
/// to this walk's filters in that order: an entry counts when its parent
/// did and the filters admit it. A directory the earlier walk did not
/// enter is walked here. A root the record cannot answer for — one the
/// earlier walk did not enter, a symlink, a path spelled with `..`, or a
/// record holding an error without a path — is walked as
/// [`collect_files`] would.
pub(super) fn replay_files(
    options: &CollectOptions<'_>,
    record: &WalkRecord,
) -> Result<Collected, SourcesError> {
    let Some((top, events)) = record.subtree(options.root) else {
        return collect_files(options);
    };
    let mut collector = Collector::new(options);
    let filter = collector.filter()?;
    collector.visit(options.root, EntryKind::Dir);
    // Paths stay in the record's spelling here and are respelled from this
    // root for the filters and the inventory, as a walk from it spells them.
    let respell = |path: &Path| match path.strip_prefix(top) {
        Ok(rest) => options.root.join(rest),
        Err(_) => path.to_path_buf(),
    };
    let mut entered: HashSet<&Path> = HashSet::from([top]);
    let parent_entered = |entered: &HashSet<&Path>, path: &Path| {
        path.parent().is_some_and(|parent| entered.contains(parent))
    };
    for event in events {
        match event {
            Event::Entry {
                path,
                is_dir,
                walked,
            } => {
                if !parent_entered(&entered, path) {
                    continue;
                }
                let here = respell(path);
                if !filter.admits(&here, *is_dir) {
                    continue;
                }
                if !is_dir {
                    collector.visit(&here, EntryKind::File);
                } else if *walked {
                    entered.insert(path);
                    collector.visit(&here, EntryKind::Dir);
                } else {
                    walk_into(&here, &filter, &mut collector, None);
                }
            },
            // A directory that could not be read reports right after its
            // own entry; anything else, under its parent.
            Event::Failed {
                path: Some(path),
                warning,
            } => {
                let shown = if record.entered(path) {
                    entered.contains(path.as_path())
                } else {
                    parent_entered(&entered, path)
                };
                if shown {
                    collector.collected.warnings.push(warning.clone());
                }
            },
            Event::Failed { path: None, .. } => {},
        }
    }
    Ok(collector.finish())
}

/// What a recorded walk met, in walk order.
#[derive(Debug, Clone)]
enum Event {
    /// A directory or source file, `walked` unless the filters turned it
    /// away.
    Entry {
        path: PathBuf,
        is_dir: bool,
        walked: bool,
    },
    /// A walk error and the path it names, if any.
    Failed {
        path: Option<PathBuf>,
        warning: SourcesWarning,
    },
}

impl Event {
    fn path(&self) -> Option<&Path> {
        match self {
            Self::Entry { path, .. } => Some(path),
            Self::Failed { path, .. } => path.as_deref(),
        }
    }
}

/// Collects [`Event`]s from a walk and its `filter_entry`.
#[derive(Debug, Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Event>>>);

impl Recorder {
    fn push(&self, event: Event) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }

    /// Records directories and source files: nothing else reaches the
    /// inventory or a warning.
    fn entry(&self, path: &Path, kind: EntryKind, walked: bool) {
        let is_dir = match kind {
            EntryKind::Dir => true,
            EntryKind::File if source_kind(path).is_some() => false,
            EntryKind::File | EntryKind::Other => return,
        };
        self.push(Event::Entry {
            path: path.to_path_buf(),
            is_dir,
            walked,
        });
    }
}

/// A walk kept by [`record_files`] for [`replay_files`].
#[derive(Debug)]
pub(crate) struct WalkRecord {
    root: PathBuf,
    events: Vec<Event>,
    /// Each directory the walk entered, relative to `root`, and its event.
    dirs: HashMap<PathBuf, usize>,
    /// Whether an error named no path, so no replay can tell where it
    /// belongs.
    pathless: bool,
}

impl WalkRecord {
    fn new(root: &Path, events: Vec<Event>) -> Self {
        let mut dirs = HashMap::new();
        let mut pathless = false;
        for (index, event) in events.iter().enumerate() {
            match event {
                Event::Entry {
                    path,
                    is_dir: true,
                    walked: true,
                } => {
                    if let Ok(rel) = path.strip_prefix(root) {
                        dirs.insert(rel.to_path_buf(), index);
                    }
                },
                Event::Failed { path: None, .. } => pathless = true,
                Event::Entry { .. } | Event::Failed { .. } => {},
            }
        }
        Self {
            root: root.to_path_buf(),
            events,
            dirs,
            pathless,
        }
    }

    fn entered(&self, path: &Path) -> bool {
        path.strip_prefix(&self.root)
            .is_ok_and(|rel| self.dirs.contains_key(rel))
    }

    /// The recorded spelling of `root` and the events below it, or `None`
    /// when the record cannot stand in for a walk from it.
    fn subtree(&self, root: &Path) -> Option<(&Path, &[Event])> {
        if self.pathless {
            return None;
        }
        let rel = root.strip_prefix(&self.root).ok()?;
        if rel.as_os_str().is_empty() {
            return None;
        }
        let &index = self.dirs.get(rel)?;
        let top = self.events[index].path()?;
        let below = &self.events[index + 1..];
        let end = below
            .iter()
            .position(|event| !event.path().is_some_and(|path| path.starts_with(top)))
            .unwrap_or(below.len());
        Some((top, &below[..end]))
    }
}

/// Validate configured entry paths.
#[must_use]
pub(super) fn validate_entries(root: &Path, entries: &[EntrySpec]) -> Vec<SourcesWarning> {
    let mut warnings = Vec::new();
    for entry in entries {
        let path = root.join(&entry.path);
        if path.is_file() {
            continue;
        }
        if path.is_dir() {
            warnings.push(SourcesWarning::EntryPathIsDirectory {
                path: entry.path.clone(),
            });
        } else {
            warnings.push(SourcesWarning::MissingEntryPath {
                path: entry.path.clone(),
            });
        }
    }
    warnings
}

/// Add a large-project warning when the threshold is exceeded.
#[must_use]
pub(super) fn large_project_warning(file_count: usize) -> Option<SourcesWarning> {
    if file_count > LARGE_PROJECT_THRESHOLD {
        Some(SourcesWarning::LargeProject { file_count })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::glob::{build_glob_set, effective_exclude};
    use crate::sources::types::{LayoutInfo, ProjectLayout};
    use std::fs;
    use tempfile::tempdir;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    #[test]
    fn own_cache_is_pruned_even_with_custom_excludes() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        write_file(&root.join(".chokkin/cache/parse/example.json"), "{}");
        write_file(&root.join(".chokkin/hidden.py"), "");
        write_file(&root.join("app.py"), "");
        let cache = SourcesCache::default();
        let patterns = effective_exclude(&["custom/**".to_owned()]);
        let exclude = cache.glob_set(&patterns).expect("excludes");
        let overrides = cache.exclude_overrides(&patterns).expect("overrides");
        let filter = Arc::new(EntryFilter {
            root: root.to_path_buf(),
            overrides,
            exclude_matcher: exclude,
            gitignore: None,
        });
        let entries: Vec<_> = configure_walker(root, filter, None)
            .build()
            .map(|entry| entry.expect("entry").into_path())
            .collect();
        assert!(entries.contains(&root.join("app.py")));
        assert!(
            !entries
                .iter()
                .any(|path| path.starts_with(root.join(".chokkin")))
        );
    }

    #[test]
    fn collect_files_honors_project_and_exclude_globs() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        write_file(&root.join("src/acme/__init__.py"), "");
        write_file(&root.join("src/acme/module.py"), "");
        write_file(&root.join("tests/test_module.py"), "");
        write_file(&root.join("local/extra.py"), "");
        write_file(&root.join("src/acme/generated/models.py"), "");

        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            inferred_globs: vec![
                "src/**/*.{py,pyi}".to_owned(),
                "tests/**/*.{py,pyi}".to_owned(),
            ],
            ..Default::default()
        };
        let project = build_glob_set(&layout.inferred_globs).expect("project globs");
        let exclude_patterns = effective_exclude(&["src/acme/generated/**".to_owned()]);
        let exclude = Arc::new(build_glob_set(&exclude_patterns).expect("exclude globs"));
        let cache = SourcesCache::default();

        let options = CollectOptions {
            root,
            project_matcher: &project,
            exclude_matcher: &exclude,
            test_matcher: &GlobSet::empty(),
            exclude_patterns: &exclude_patterns,
            respect_gitignore: false,
            gitignore: None,
            production: false,
            cache: &cache,
        };
        let files = collect_files(&options).expect("collect").files;
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"src/acme/module.py"));
        assert!(paths.contains(&"tests/test_module.py"));
        assert!(!paths.contains(&"local/extra.py"));
        assert!(!paths.contains(&"src/acme/generated/models.py"));
    }

    #[test]
    fn collect_files_filters_non_production_contexts() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        write_file(&root.join("src/acme/__init__.py"), "");
        write_file(&root.join("src/acme/module.py"), "");
        write_file(&root.join("tests/test_module.py"), "");

        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            inferred_globs: vec![
                "src/**/*.{py,pyi}".to_owned(),
                "tests/**/*.{py,pyi}".to_owned(),
            ],
            ..Default::default()
        };
        let project = build_glob_set(&layout.inferred_globs).expect("project globs");
        let exclude_patterns = effective_exclude(&[]);
        let exclude = Arc::new(build_glob_set(&exclude_patterns).expect("exclude globs"));
        let cache = SourcesCache::default();

        let options = CollectOptions {
            root,
            project_matcher: &project,
            exclude_matcher: &exclude,
            test_matcher: &GlobSet::empty(),
            exclude_patterns: &exclude_patterns,
            respect_gitignore: false,
            gitignore: None,
            production: true,
            cache: &cache,
        };
        let collected = collect_files(&options).expect("collect");
        let paths: Vec<_> = collected
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert!(paths.contains(&"src/acme/module.py"));
        assert!(!paths.contains(&"tests/test_module.py"));
        // The dropped tests stay at hand for CHK006 (#844).
        assert_eq!(
            collected.held_tests,
            [DiscoveredFile {
                path: "tests/test_module.py".to_owned(),
                kind: FileKind::Python,
                context: FileContext::Test,
            }]
        );
    }

    #[test]
    fn collect_files_prunes_excluded_directories() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        write_file(&root.join("src/acme/module.py"), "");
        write_file(&root.join(".venv/lib/site.py"), "");

        let layout = LayoutInfo {
            inferred_globs: vec!["**/*.{py,pyi}".to_owned()],
            ..Default::default()
        };
        let project = build_glob_set(&layout.inferred_globs).expect("project globs");
        let exclude_patterns = effective_exclude(&[".venv/**".to_owned()]);
        let exclude = Arc::new(build_glob_set(&exclude_patterns).expect("exclude globs"));
        let cache = SourcesCache::default();

        let options = CollectOptions {
            root,
            project_matcher: &project,
            exclude_matcher: &exclude,
            test_matcher: &GlobSet::empty(),
            exclude_patterns: &exclude_patterns,
            respect_gitignore: false,
            gitignore: None,
            production: false,
            cache: &cache,
        };
        let files = collect_files(&options).expect("collect").files;
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"src/acme/module.py"));
        assert!(!paths.contains(&".venv/lib/site.py"));
    }

    static NO_GLOBS: GlobSet = GlobSet::empty();

    fn plain_options<'a>(
        root: &'a Path,
        project: &'a GlobSet,
        exclude: &'a Arc<GlobSet>,
        exclude_patterns: &'a [String],
        cache: &'a SourcesCache,
    ) -> CollectOptions<'a> {
        CollectOptions {
            root,
            project_matcher: project,
            exclude_matcher: exclude,
            test_matcher: &NO_GLOBS,
            exclude_patterns,
            respect_gitignore: false,
            gitignore: None,
            production: false,
            cache,
        }
    }

    fn file_paths(collected: &Collected) -> Vec<&str> {
        collected
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect()
    }

    /// A replay answers from the record: what changed on disk since shows
    /// only under a directory the root walk did not enter (#843).
    #[test]
    fn replay_reads_only_the_directories_the_record_lacks() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        write_file(&root.join("pkg/kept.py"), "");
        write_file(&root.join("pkg/removed.py"), "");
        write_file(&root.join("pkg/skipped/old.py"), "");
        let project = build_glob_set(&["**/*.py".to_owned()]).expect("project globs");
        let cache = SourcesCache::default();
        let root_patterns = effective_exclude(&["pkg/skipped/**".to_owned()]);
        let root_exclude = cache.glob_set(&root_patterns).expect("excludes");
        let options = plain_options(root, &project, &root_exclude, &root_patterns, &cache);
        let (collected, record) = record_files(&options).expect("record");
        assert_eq!(file_paths(&collected), ["pkg/kept.py", "pkg/removed.py"]);

        fs::remove_file(root.join("pkg/removed.py")).expect("remove");
        write_file(&root.join("pkg/added.py"), "");
        write_file(&root.join("pkg/skipped/new.py"), "");
        let member = root.join("pkg");
        let patterns = effective_exclude(&[]);
        let exclude = cache.glob_set(&patterns).expect("excludes");
        let options = plain_options(&member, &project, &exclude, &patterns, &cache);
        let replayed = replay_files(&options, &record).expect("replay");
        assert_eq!(
            file_paths(&replayed),
            ["kept.py", "removed.py", "skipped/new.py", "skipped/old.py"]
        );
    }

    /// A recorded error shows in a replay when the replay would have met
    /// it: a directory that could not be read under its own entry, any
    /// other path under its parent. An error without a path sends the
    /// replay back to a walk (#843).
    #[test]
    fn replayed_errors_follow_what_the_member_enters() {
        let root = Path::new("/repo");
        let failed = |path: &str| Event::Failed {
            path: Some(root.join(path)),
            warning: SourcesWarning::PathUnreadable {
                path: path.to_owned(),
                reason: "denied".to_owned(),
            },
        };
        let dir = |path: &str| Event::Entry {
            path: root.join(path),
            is_dir: true,
            walked: true,
        };
        let record = WalkRecord::new(
            root,
            vec![
                dir(""),
                dir("m"),
                dir("m/secret"),
                failed("m/secret"),
                dir("m/dropped"),
                failed("m/dropped"),
                failed("m/dropped/child"),
                failed("m/odd"),
                dir("other"),
                failed("other/secret"),
            ],
        );
        let project = build_glob_set(&["**/*.py".to_owned()]).expect("project globs");
        let cache = SourcesCache::default();
        let patterns = effective_exclude(&["dropped/**".to_owned()]);
        let exclude = cache.glob_set(&patterns).expect("excludes");
        let member = root.join("m");
        let options = plain_options(&member, &project, &exclude, &patterns, &cache);
        let replayed = replay_files(&options, &record).expect("replay");
        let reported: Vec<_> = replayed
            .warnings
            .iter()
            .map(|warning| match warning {
                SourcesWarning::PathUnreadable { path, .. } => path.as_str(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(reported, ["m/secret", "m/odd"]);

        let pathless = WalkRecord::new(
            root,
            vec![
                dir(""),
                dir("m"),
                Event::Failed {
                    path: None,
                    warning: SourcesWarning::PathUnreadable {
                        path: "/repo".to_owned(),
                        reason: "gone".to_owned(),
                    },
                },
            ],
        );
        assert!(pathless.subtree(&member).is_none());
        assert!(record.subtree(&member).is_some());
        assert!(record.subtree(root).is_none());
        assert!(record.subtree(&root.join("m/../m")).is_none());
    }

    #[test]
    fn validate_entries_warns_on_missing_path() {
        let temp = tempdir().expect("tempdir");
        let warnings = validate_entries(
            temp.path(),
            &[EntrySpec {
                path: "missing.py".to_owned(),
                symbol: None,
            }],
        );
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            SourcesWarning::MissingEntryPath { .. }
        ));
    }

    /// The root walk and a member walk meet the same directory at different
    /// depths and roots, and name it the same way, so the reporter keeps one
    /// line (#900). An error without a path, or an ignore file that does not
    /// parse, still names the walk's root.
    #[test]
    fn walk_error_warning_names_the_path_that_failed() {
        let native = |p: &str| {
            Path::new(p)
                .components()
                .collect::<PathBuf>()
                .to_string_lossy()
                .into_owned()
        };
        let denied = |depth, path| ignore::Error::WithDepth {
            depth,
            err: Box::new(ignore::Error::WithPath {
                path,
                err: Box::new(ignore::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "denied",
                ))),
            }),
        };
        let expected = SourcesWarning::PathUnreadable {
            path: native("/repo/packages/core/secret"),
            reason: "denied".to_owned(),
        };

        let root_walk = Path::new("/repo")
            .join("packages")
            .join("core")
            .join("secret");
        let member_walk = Path::new("/repo").join("packages/core").join("secret");

        assert_eq!(
            walk_error_warning(Path::new("/repo"), &denied(3, root_walk)),
            expected
        );
        assert_eq!(
            walk_error_warning(Path::new("/repo/packages/core"), &denied(1, member_walk)),
            expected
        );
        let pathless = ignore::Error::Io(std::io::Error::other("gone"));
        assert_eq!(
            walk_error_warning(Path::new("/repo"), &pathless),
            SourcesWarning::PathUnreadable {
                path: native("/repo"),
                reason: "gone".to_owned(),
            }
        );
        let unparsable = ignore::Error::WithPath {
            path: PathBuf::from("/.gitignore"),
            err: Box::new(ignore::Error::WithLineNumber {
                line: 1,
                err: Box::new(ignore::Error::Io(std::io::Error::other("bad glob"))),
            }),
        };
        assert_eq!(
            walk_error_warning(Path::new("/repo"), &unparsable),
            SourcesWarning::PathUnreadable {
                path: native("/repo"),
                reason: "/.gitignore: line 1: bad glob".to_owned(),
            }
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn large_project_warning_triggers_exactly_above_threshold(count in 0usize..30_000) {
                let warning = large_project_warning(count);
                prop_assert_eq!(warning.is_some(), count > LARGE_PROJECT_THRESHOLD);
                if let Some(SourcesWarning::LargeProject { file_count }) = warning {
                    prop_assert_eq!(file_count, count);
                }
            }

            #[test]
            fn validate_entries_reports_each_missing_entry(
                names in prop::collection::btree_set("[a-z]{1,10}", 0..5),
            ) {
                let temp = tempdir().expect("tempdir");
                let entries: Vec<EntrySpec> = names
                    .iter()
                    .map(|name| EntrySpec {
                        path: format!("{name}.py"),
                        symbol: None,
                    })
                    .collect();
                let warnings = validate_entries(temp.path(), &entries);
                prop_assert_eq!(warnings.len(), entries.len());
                let all_missing = warnings
                    .iter()
                    .all(|warning| matches!(warning, SourcesWarning::MissingEntryPath { .. }));
                prop_assert!(all_missing);
            }
        }
    }
}
