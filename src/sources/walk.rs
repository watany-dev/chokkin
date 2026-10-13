//! Directory walking with glob, exclude, and gitignore filters.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use globset::GlobSet;
use ignore::WalkBuilder;
use ignore::overrides::Override;

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
                return !should_prune_dir(&rel_str, &exclude_matcher);
            }
            true
        });
    builder
}

/// Collect Python-related files under `root`.
pub(super) fn collect_files(
    options: &CollectOptions<'_>,
) -> Result<(Vec<DiscoveredFile>, Vec<SourcesWarning>), SourcesError> {
    let root = options.root.to_path_buf();
    let project_matcher = options.project_matcher;
    let exclude_matcher = options.exclude_matcher;
    let test_matcher = options.test_matcher;
    let production = options.production;
    let gitignore = options.gitignore;
    let cache = options.cache;

    let mut files = Vec::new();
    let mut warnings = Vec::new();

    let overrides = cache.exclude_overrides(options.exclude_patterns)?;
    let filter = options.respect_gitignore.then(|| {
        let ancestors = cache.ancestors(&root).map(Arc::new);
        if let Some(reason) = ancestors.as_deref().and_then(Ancestors::error) {
            warnings.push(SourcesWarning::PathUnreadable {
                path: root.to_string_lossy().into_owned(),
                reason,
            });
        }
        GitignoreFilter::new(&root, cache, ancestors)
    });
    let builder = configure_walker(&root, overrides, Arc::clone(exclude_matcher), filter);

    for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(SourcesWarning::PathUnreadable {
                    path: root.to_string_lossy().into_owned(),
                    reason: error.to_string(),
                });
                continue;
            },
        };

        let path = entry.path();
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir()
            && options.respect_gitignore
            && let Some(reason) = cache.dir(path).error()
        {
            warnings.push(SourcesWarning::PathUnreadable {
                path: path.to_string_lossy().into_owned(),
                reason,
            });
        }
        if !file_type.is_file() {
            continue;
        }

        let kind = match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("py") => FileKind::Python,
            Some(ext) if ext.eq_ignore_ascii_case("pyi") => FileKind::Stub,
            Some(ext) if ext.eq_ignore_ascii_case("ipynb") => FileKind::Notebook,
            _ => continue,
        };

        let Ok(rel) = path.strip_prefix(&root) else {
            continue;
        };
        let rel_str = normalize_rel_path(rel);

        if !project_matcher.is_match(&rel_str) || exclude_matcher.is_match(&rel_str) {
            continue;
        }

        if gitignore.is_some_and(|gitignore| gitignore.gitignore_ignores_file(rel)) {
            continue;
        }

        let context = if test_matcher.is_match(&rel_str) {
            FileContext::Test
        } else {
            assign_file_context(&rel_str)
        };
        if production && !context.is_included_in_production() {
            continue;
        }

        files.push(DiscoveredFile {
            path: rel_str,
            kind,
            context,
        });
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((files, warnings))
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
        let entries: Vec<_> = configure_walker(root, overrides, exclude, None)
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
        let (files, _) = collect_files(&options).expect("collect");
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
        let (files, _) = collect_files(&options).expect("collect");
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"src/acme/module.py"));
        assert!(!paths.contains(&"tests/test_module.py"));
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
        let (files, _) = collect_files(&options).expect("collect");
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
