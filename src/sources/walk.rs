//! Directory walking with glob, exclude, and gitignore filters.

use std::collections::BTreeSet;
use std::fs::FileType;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use globset::GlobSet;
use ignore::overrides::Override;
use ignore::{DirEntry, WalkBuilder};

use crate::config::EntrySpec;
use crate::path_util::{lowercase_extension, normalize_rel_path};

use super::cache::{Ancestors, DirIgnore, RootLoad, SourcesCache, WalkIgnore, gitignore_skips};
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
    /// Whether to drop non-runtime contexts.
    pub production: bool,
    /// Ignore files and globs parsed by earlier walks.
    pub cache: &'a SourcesCache,
    /// When set, the only directories the walk enters, root-relative: a walk
    /// for named files leaves the rest of the tree alone (#932).
    pub dirs: Option<&'a BTreeSet<String>>,
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
    /// Root-relative files whose commands the plugins read outside the
    /// config-scan cache, sorted: Dockerfiles below the root, whose server
    /// apps are entries (#1061), and shell script and supervisor config
    /// candidates (#1240). The project globs do not apply; the exclusions
    /// and ignore rules do.
    pub command_files: Vec<String>,
    /// What the walk could not read.
    pub warnings: Vec<SourcesWarning>,
}

/// The warning for a root `.gitignore` the walk could not read. The walk
/// reports the lines that do not parse; its partial rules still apply.
pub(super) fn root_gitignore_warning(root: &Path, cache: &SourcesCache) -> Option<SourcesWarning> {
    matches!(cache.dir(root).root_load(), RootLoad::Unreadable).then(|| {
        SourcesWarning::GitignoreUnreadable {
            path: GITIGNORE_PATH.to_owned(),
        }
    })
}

fn should_prune_dir(rel_str: &str, exclude_matcher: &GlobSet) -> bool {
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

fn configure_walker(
    root: &Path,
    overrides: Arc<Override>,
    exclude_matcher: Arc<GlobSet>,
    gitignore: Option<GitignoreFilter>,
    dirs: Option<BTreeSet<String>>,
) -> WalkBuilder {
    let root_for_filter = root.to_path_buf();

    let mut builder = WalkBuilder::new(root);
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
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(&root_for_filter) else {
                return true;
            };
            let is_dir = entry
                .file_type()
                .is_some_and(|file_type| file_type.is_dir());
            if overrides.matched(rel, is_dir).is_ignore() {
                return false;
            }
            if gitignore
                .as_ref()
                .is_some_and(|filter| filter.skips(entry.path(), is_dir))
            {
                return false;
            }
            if is_dir {
                let rel_str = normalize_rel_path(rel);
                let outside = dirs
                    .as_ref()
                    .is_some_and(|dirs| !rel_str.is_empty() && !dirs.contains(&rel_str));
                return !outside && !should_prune_dir(&rel_str, &exclude_matcher);
            }
            true
        });
    builder
}

/// Ignore file errors of a walked directory, as `ignore` attaches them to
/// its entry, and a `.gitignore` below the walk root that `ignore` could not
/// read and dropped (#915). The root's own is [`root_gitignore_warning`]'s.
fn dir_warnings(
    root: &Path,
    cache: &SourcesCache,
    entry: &DirEntry,
    file_type: FileType,
    warnings: &mut Vec<SourcesWarning>,
) {
    let path = entry.path();
    // A symlinked root keeps its link type but is walked as a directory.
    if !(file_type.is_dir() || (entry.depth() == 0 && path.is_dir())) {
        return;
    }
    let dir = cache.dir(path);
    for error in dir.errors() {
        push_walk_error(path, error, warnings);
    }
    if entry.depth() > 0
        && dir.root_load() == RootLoad::Unreadable
        && let Some(warning) = nested_gitignore_unreadable(root, path)
    {
        warnings.push(warning);
    }
}

/// [`SourcesWarning::GitignoreUnreadable`] for the `.gitignore` of `dir`,
/// named relative to the walk root like the root's own.
fn nested_gitignore_unreadable(root: &Path, dir: &Path) -> Option<SourcesWarning> {
    let rel = dir.strip_prefix(root).ok()?;
    Some(SourcesWarning::GitignoreUnreadable {
        path: normalize_rel_path(&rel.join(GITIGNORE_PATH)),
    })
}

/// A walk error as warnings on the paths that failed, so the root walk and a
/// member walk that meet the same directory or ignore file say the same
/// thing (#900, #903): an I/O error names the path it could not read, and an
/// ignore file that does not parse names the file, one warning per line.
/// A member root joins `packages/core` onto the root, so a path is rebuilt
/// from its components to spell it with the platform's separator either way.
/// An error without a path names `fallback`, the walk root or the directory
/// whose ignore files failed.
fn push_walk_error(fallback: &Path, error: &ignore::Error, warnings: &mut Vec<SourcesWarning>) {
    let native = |path: &Path| {
        path.components()
            .collect::<PathBuf>()
            .to_string_lossy()
            .into_owned()
    };
    match error {
        ignore::Error::Partial(errors) => {
            for error in errors {
                push_walk_error(fallback, error, warnings);
            }
        },
        // The depth differs between walks.
        ignore::Error::WithDepth { err, .. } => push_walk_error(fallback, err, warnings),
        ignore::Error::WithPath { path, err } => warnings.push(SourcesWarning::PathUnreadable {
            path: native(path),
            reason: err.to_string(),
        }),
        _ => warnings.push(SourcesWarning::PathUnreadable {
            path: native(fallback),
            reason: error.to_string(),
        }),
    }
}

/// Symlinks the walk did not follow, with why (#986).
#[derive(Default)]
struct SkippedSymlinks(Vec<SourcesWarning>);

impl SkippedSymlinks {
    /// Record `entry` if it is a link [`skipped_symlink`] names.
    fn check(
        &mut self,
        root: &Path,
        entry: &DirEntry,
        exclude_matcher: &GlobSet,
        ignore: Option<&WalkIgnore>,
    ) {
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_symlink())
        {
            self.0
                .extend(skipped_symlink(root, entry, exclude_matcher, ignore));
        }
    }

    /// Append the links in path order: the walk meets them in directory
    /// order, which the file system picks.
    fn finish(mut self, warnings: &mut Vec<SourcesWarning>) {
        self.0.sort_by_cached_key(ToString::to_string);
        warnings.append(&mut self.0);
    }
}

/// Source files whose paths are not UTF-8 (#1146).
#[derive(Default)]
struct NonUtf8Paths(Vec<SourcesWarning>);

impl NonUtf8Paths {
    /// Record `rel` if it is not UTF-8. Its lossy `rel_str` names no file on
    /// disk, and Python cannot import the module, so it is skipped out loud.
    fn skips(&mut self, rel: &Path, rel_str: &str) -> bool {
        if rel.to_str().is_some() {
            return false;
        }
        self.0.push(SourcesWarning::NonUtf8Path {
            path: rel_str.to_owned(),
        });
        true
    }

    /// Append the paths in path order, not the walk's directory order.
    fn finish(mut self, warnings: &mut Vec<SourcesWarning>) {
        self.0.sort_by_cached_key(ToString::to_string);
        warnings.append(&mut self.0);
    }
}

/// Why the walk did not follow the symlink at `entry`, or `None` when the
/// skip needs no word (#986). The walk follows no link: one inside the root
/// is read at its target's own path, so it stays silent. One that leaves the
/// root, loops, or leads nowhere may be code the user meant to analyze, so
/// it is named when it could have held some: a source file name, a
/// directory, or a [`bare_name`] that cannot be resolved, so a dangling
/// `.gitignore` stays the [`SourcesWarning::GitignoreUnreadable`] it is.
/// The root itself is walked through its link.
fn skipped_symlink(
    root: &Path,
    entry: &DirEntry,
    exclude_matcher: &GlobSet,
    ignore: Option<&WalkIgnore>,
) -> Option<SourcesWarning> {
    let path = entry.path();
    if entry.depth() == 0 {
        return None;
    }
    let rel = path.strip_prefix(root).ok()?;
    let rel_str = normalize_rel_path(rel);
    // The walk dropped the ignored links, seeing each as a file; a link to a
    // directory (a symlinked `venv`) answers to `venv/` too.
    if should_prune_dir(&rel_str, exclude_matcher)
        || ignore.is_some_and(|ignore| ignore.skips(path, true))
    {
        return None;
    }
    let named_source = source_kind(path).is_some();
    let reason = match path.metadata() {
        Ok(meta) => {
            let target = std::fs::canonicalize(path).ok()?;
            let root = std::fs::canonicalize(root).ok()?;
            if !(named_source || meta.is_dir()) || target.starts_with(&root) {
                return None;
            }
            "it points outside the project root".to_owned()
        },
        Err(_) if !named_source && !bare_name(path) => return None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            "it points to nothing".to_owned()
        },
        // ELOOP has no stable `ErrorKind`, so the chain is walked instead.
        Err(_) if forms_loop(path) => "it forms a loop".to_owned(),
        Err(error) => error.to_string(),
    };
    Some(SourcesWarning::SymlinkSkipped {
        path: rel_str,
        reason,
    })
}

/// A name with neither an extension nor a leading dot, as a directory's
/// usually is.
fn bare_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| !name.contains('.'))
}

/// A chain of links from `path` that is still a link after as many hops as
/// the OS follows (40 on Linux) before it calls the chain a loop.
fn forms_loop(path: &Path) -> bool {
    let mut current = path.to_path_buf();
    for _ in 0..=40 {
        let Ok(target) = std::fs::read_link(&current) else {
            return false;
        };
        current = match current.parent() {
            Some(parent) => parent.join(target),
            None => target,
        };
    }
    true
}

pub(crate) fn is_dockerfile(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".dockerignore") {
        return false;
    }
    matches!(lower.as_str(), "dockerfile" | "containerfile")
        || lower.ends_with(".dockerfile")
        || lower.starts_with("dockerfile.")
        || lower.starts_with("containerfile.")
}

/// A `.txt` / `.in` file in a directory named `requirements` at any depth
/// (ansible's `test/lib/ansible_test/_data/requirements/units.txt`), which
/// a runner installs from with `pip install -r` (#1273). The fixed
/// `requirements/` and `tests/requirements/` are read by the manifest too.
pub(crate) fn is_requirements_dir_file(rel: &Path) -> bool {
    let extension = rel.extension().and_then(|ext| ext.to_str());
    extension.is_some_and(|ext| ext.eq_ignore_ascii_case("txt") || ext.eq_ignore_ascii_case("in"))
        && rel
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|dir| dir == "requirements")
}

/// `entry`, at `rel`, as a command file that is neither excluded nor ignored:
/// a Dockerfile below the root (#1061), a file named like a shell script
/// (`.sh` / `.bash` / `.zsh` / `.ksh`, or no extension) outside the root's
/// `scripts/` and `bin/`, which the config scan reads, a supervisor config
/// (`.conf`), or a requirements file in a `requirements` directory (#1273).
/// The plugins read the contents and drop what is neither (#1240).
fn command_file(entry: &DirEntry, rel: &Path, exclude_matcher: &GlobSet) -> Option<String> {
    let path = entry.path();
    let wanted = if is_dockerfile(path) {
        entry.depth() >= 2
    } else {
        let extension = path.extension().and_then(|ext| ext.to_str());
        let is = |wanted: &[&str]| {
            extension.is_some_and(|ext| wanted.iter().any(|name| ext.eq_ignore_ascii_case(name)))
        };
        let named = extension.is_none() || is(&["sh", "bash", "zsh", "ksh", "conf"]);
        let config_scanned = !is(&["conf"])
            && entry.depth() == 2
            && rel
                .parent()
                .is_some_and(|dir| dir == Path::new("scripts") || dir == Path::new("bin"));
        named && !config_scanned || is_requirements_dir_file(rel)
    };
    if !wanted {
        return None;
    }
    let rel_str = normalize_rel_path(rel);
    (!exclude_matcher.is_match(&rel_str)).then_some(rel_str)
}

/// Whether `matcher` takes the source file at `rel_str`, also under its
/// extension in lowercase: `source_kind` collects `helper.PY` as Python, and
/// the default `**/*.py` globs must not drop it again (#1170).
pub(super) fn matches_source(matcher: &GlobSet, rel_str: &str) -> bool {
    matcher.is_match(rel_str) || {
        let lower = lowercase_extension(rel_str);
        lower != rel_str && matcher.is_match(lower.as_ref())
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

fn walker(
    options: &CollectOptions<'_>,
    warnings: &mut Vec<SourcesWarning>,
) -> Result<WalkBuilder, SourcesError> {
    let root = options.root;
    let overrides = options.cache.exclude_overrides(options.exclude_patterns)?;
    let filter = options.respect_gitignore.then(|| {
        let ancestors = options.cache.ancestors(root).map(Arc::new);
        for error in ancestors.iter().flat_map(|ancestors| ancestors.errors()) {
            push_walk_error(root, error, warnings);
        }
        GitignoreFilter::new(root, options.cache, ancestors)
    });
    Ok(configure_walker(
        root,
        overrides,
        Arc::clone(options.exclude_matcher),
        filter,
        options.dirs.cloned(),
    ))
}

/// Collect Python-related files under `root`.
pub(super) fn collect_files(options: &CollectOptions<'_>) -> Result<Collected, SourcesError> {
    let root = options.root.to_path_buf();
    let project_matcher = options.project_matcher;
    let exclude_matcher = options.exclude_matcher;
    let test_matcher = options.test_matcher;
    let production = options.production;
    let cache = options.cache;

    let mut files = Vec::new();
    let mut held_tests = Vec::new();
    let mut command_files = Vec::new();
    let mut warnings = Vec::new();
    let mut symlinks = SkippedSymlinks::default();
    let mut non_utf8 = NonUtf8Paths::default();
    let ignore = options
        .respect_gitignore
        .then(|| WalkIgnore::new(cache, &root));

    let builder = walker(options, &mut warnings)?;

    for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                push_walk_error(&root, &error, &mut warnings);
                continue;
            },
        };

        let path = entry.path();
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if options.respect_gitignore {
            dir_warnings(&root, cache, &entry, file_type, &mut warnings);
        }
        symlinks.check(&root, &entry, exclude_matcher, ignore.as_ref());
        if !file_type.is_file() {
            continue;
        }

        let Ok(rel) = path.strip_prefix(&root) else {
            continue;
        };
        let Some(kind) = source_kind(path) else {
            command_files.extend(command_file(&entry, rel, exclude_matcher));
            continue;
        };
        let rel_str = normalize_rel_path(rel);

        if !matches_source(project_matcher, &rel_str) || matches_source(exclude_matcher, &rel_str) {
            continue;
        }

        if non_utf8.skips(rel, &rel_str) {
            continue;
        }

        let context = if matches_source(test_matcher, &rel_str) {
            FileContext::Test
        } else {
            assign_file_context(&rel_str)
        };
        let file = DiscoveredFile {
            path: rel_str,
            kind,
            context,
        };
        if production && !context.is_included_in_production() {
            if context == FileContext::Test {
                held_tests.push(file);
            }
            continue;
        }
        files.push(file);
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    held_tests.sort_by(|left, right| left.path.cmp(&right.path));
    command_files.sort();
    non_utf8.finish(&mut warnings);
    symlinks.finish(&mut warnings);
    Ok(Collected {
        files,
        held_tests,
        command_files,
        warnings,
    })
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
    use crate::sources::types::LayoutInfo;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    #[test]
    fn dockerfile_names_are_matched_case_insensitively() {
        for name in [
            "Dockerfile",
            "dockerfile",
            "api.Dockerfile",
            "Dockerfile.dev",
            "Containerfile",
        ] {
            assert!(is_dockerfile(Path::new(name)), "{name}");
        }
        assert!(!is_dockerfile(Path::new("Dockerfile.dockerignore")));
        assert!(!is_dockerfile(Path::new("docker-compose.yml")));
    }

    #[test]
    fn source_kind_collects_python_extensions_only() {
        let kind = |path: &str| source_kind(Path::new(path));
        assert_eq!(kind("a.PY"), Some(FileKind::Python));
        assert_eq!(kind("a.pyi"), Some(FileKind::Stub));
        assert_eq!(kind("a.ipynb"), Some(FileKind::Notebook));
        assert_eq!(kind("setup.cfg"), None);
        assert_eq!(kind("Makefile"), None);
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
        let entries: Vec<_> = configure_walker(root, overrides, exclude, None, None)
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
    fn a_dirs_walk_enters_only_the_named_directories() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        write_file(&root.join("docs/conf.py"), "");
        write_file(&root.join("src/app/module.py"), "");
        write_file(&root.join("manage.py"), "");
        let cache = SourcesCache::default();
        let patterns = effective_exclude(&[]);
        let exclude = cache.glob_set(&patterns).expect("excludes");
        let overrides = cache.exclude_overrides(&patterns).expect("overrides");
        let dirs = BTreeSet::from(["docs".to_owned()]);
        let entries: Vec<_> = configure_walker(root, overrides, exclude, None, Some(dirs))
            .build()
            .map(|entry| entry.expect("entry").into_path())
            .collect();
        assert!(entries.contains(&root.join("docs/conf.py")));
        assert!(entries.contains(&root.join("manage.py")));
        assert!(!entries.contains(&root.join("src")));
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
        for dockerfile in [
            "Dockerfile",
            "svc/Dockerfile",
            "local/api.dockerfile",
            "src/acme/generated/Dockerfile",
        ] {
            write_file(&root.join(dockerfile), "FROM python\n");
        }
        for file in [
            "entrypoint.sh",
            "svc/supervisord.conf",
            "svc/start-worker",
            "svc/notes.md",
            "scripts/release.sh",
            "bin/supervisord.conf",
            "src/acme/generated/run.sh",
        ] {
            write_file(&root.join(file), "#!/bin/sh\n");
        }

        let layout = LayoutInfo {
            inferred_globs: vec![
                "src/**/*.{py,pyi}".to_owned(),
                "tests/**/*.{py,pyi}".to_owned(),
            ],
            ..LayoutInfo::new("src", vec!["acme".to_owned()])
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
            production: false,
            cache: &cache,
            dirs: None,
        };
        let collected = collect_files(&options).expect("collect");
        let paths: Vec<_> = collected
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert!(paths.contains(&"src/acme/module.py"));
        assert!(paths.contains(&"tests/test_module.py"));
        assert!(!paths.contains(&"local/extra.py"));
        assert!(!paths.contains(&"src/acme/generated/models.py"));
        // Dockerfiles below the root ignore the project globs, not the
        // exclusions (#1061); the root's own are read by `task_files`. So do
        // shell script and supervisor config candidates, but for the ones
        // the config scan reads under `scripts/` and `bin/` (#1240).
        assert_eq!(
            collected.command_files,
            [
                "bin/supervisord.conf",
                "entrypoint.sh",
                "local/api.dockerfile",
                "svc/Dockerfile",
                "svc/start-worker",
                "svc/supervisord.conf",
            ]
        );
    }

    #[test]
    fn collect_files_filters_non_production_contexts() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        write_file(&root.join("src/acme/__init__.py"), "");
        write_file(&root.join("src/acme/module.py"), "");
        write_file(&root.join("tests/test_module.py"), "");

        let layout = LayoutInfo {
            inferred_globs: vec![
                "src/**/*.{py,pyi}".to_owned(),
                "tests/**/*.{py,pyi}".to_owned(),
            ],
            ..LayoutInfo::new("src", vec!["acme".to_owned()])
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
            production: true,
            cache: &cache,
            dirs: None,
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
            production: false,
            cache: &cache,
            dirs: None,
        };
        let files = collect_files(&options).expect("collect").files;
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"src/acme/module.py"));
        assert!(!paths.contains(&".venv/lib/site.py"));
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

    /// The root walk and a member walk meet the same directory or ignore
    /// file at different depths and roots, and name it the same way, so the
    /// reporter keeps one line (#900, #903). An error without a path names
    /// the fallback.
    #[test]
    fn push_walk_error_names_the_path_that_failed() {
        let native = |p: &str| {
            Path::new(p)
                .components()
                .collect::<PathBuf>()
                .to_string_lossy()
                .into_owned()
        };
        let warnings = |fallback: &str, error: &ignore::Error| {
            let mut warnings = Vec::new();
            push_walk_error(Path::new(fallback), error, &mut warnings);
            warnings
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
        let expected = vec![SourcesWarning::PathUnreadable {
            path: native("/repo/packages/core/secret"),
            reason: "denied".to_owned(),
        }];

        let root_walk = Path::new("/repo")
            .join("packages")
            .join("core")
            .join("secret");
        let member_walk = Path::new("/repo").join("packages/core").join("secret");

        assert_eq!(warnings("/repo", &denied(3, root_walk)), expected);
        assert_eq!(
            warnings("/repo/packages/core", &denied(1, member_walk)),
            expected
        );
        let pathless = ignore::Error::Io(std::io::Error::other("gone"));
        assert_eq!(
            warnings("/repo", &pathless),
            vec![SourcesWarning::PathUnreadable {
                path: native("/repo"),
                reason: "gone".to_owned(),
            }]
        );
        let bad_line = |line, text: &str| ignore::Error::WithPath {
            path: PathBuf::from("/repo/.gitignore"),
            err: Box::new(ignore::Error::WithLineNumber {
                line,
                err: Box::new(ignore::Error::Io(std::io::Error::other(text.to_owned()))),
            }),
        };
        let unparsable =
            ignore::Error::Partial(vec![bad_line(1, "bad glob"), bad_line(3, "worse")]);
        let expected = vec![
            SourcesWarning::PathUnreadable {
                path: native("/repo/.gitignore"),
                reason: "line 1: bad glob".to_owned(),
            },
            SourcesWarning::PathUnreadable {
                path: native("/repo/.gitignore"),
                reason: "line 3: worse".to_owned(),
            },
        ];
        assert_eq!(warnings("/repo", &unparsable), expected);
        assert_eq!(warnings("/repo/packages/core", &unparsable), expected);
    }

    /// The sampled property below rarely lands on the threshold itself.
    #[test]
    fn large_project_warning_starts_above_the_threshold() {
        assert_eq!(large_project_warning(LARGE_PROJECT_THRESHOLD), None);
        assert_eq!(
            large_project_warning(LARGE_PROJECT_THRESHOLD + 1),
            Some(SourcesWarning::LargeProject {
                file_count: LARGE_PROJECT_THRESHOLD + 1,
            })
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
