//! Directory walking with glob, exclude, and gitignore filters.

use std::path::Path;

use globset::GlobSet;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::overrides::OverrideBuilder;
use ignore::{DirEntry, WalkBuilder};

use crate::config::EntrySpec;

use super::context::assign_file_context;
use super::error::SourcesError;
use super::types::{DiscoveredFile, FileKind, LayoutInfo};
use super::warnings::SourcesWarning;

const LARGE_PROJECT_THRESHOLD: usize = 10_000;
const GITIGNORE_PATH: &str = ".gitignore";

/// Options for [`collect_files`].
pub struct CollectOptions<'a> {
    /// Project root directory.
    pub root: &'a Path,
    /// Globs selecting project files.
    pub project_matcher: &'a GlobSet,
    /// Globs excluding files from analysis.
    pub exclude_matcher: &'a GlobSet,
    /// Raw exclude patterns (for directory pruning via overrides).
    pub exclude_patterns: &'a [String],
    /// Whether to respect `.gitignore` files during the walk.
    pub respect_gitignore: bool,
    /// Optional root `.gitignore` matcher for post-filtering.
    pub gitignore: Option<&'a Gitignore>,
    /// Whether to drop non-runtime contexts.
    pub production: bool,
    /// Inferred layout information.
    pub layout: &'a LayoutInfo,
}

/// Load `.gitignore` from the project root when present.
pub fn load_gitignore(root: &Path) -> (Option<Gitignore>, Option<SourcesWarning>) {
    let gitignore_path = root.join(".gitignore");
    if !gitignore_path.is_file() {
        return (None, None);
    }

    let mut builder = GitignoreBuilder::new(root);
    if builder.add(&gitignore_path).is_some() {
        return (
            None,
            Some(SourcesWarning::GitignoreUnreadable {
                path: GITIGNORE_PATH.to_owned(),
            }),
        );
    }

    match builder.build() {
        Ok(gitignore) => (Some(gitignore), None),
        Err(_error) => (
            None,
            Some(SourcesWarning::GitignoreUnreadable {
                path: GITIGNORE_PATH.to_owned(),
            }),
        ),
    }
}

fn build_exclude_overrides(
    root: &Path,
    patterns: &[String],
) -> Result<ignore::overrides::Override, SourcesError> {
    let mut builder = OverrideBuilder::new(root);
    for pattern in patterns {
        let ignore_glob = format!("!{pattern}");
        builder
            .add(&ignore_glob)
            .map_err(|error| SourcesError::InvalidGlob {
                pattern: pattern.clone(),
                reason: error.to_string(),
            })?;
    }
    builder.build().map_err(|error| SourcesError::InvalidGlob {
        pattern: String::new(),
        reason: error.to_string(),
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

fn walk_entry_warning(entry: &DirEntry) -> Option<SourcesWarning> {
    entry.error().map(|error| SourcesWarning::PathUnreadable {
        path: entry.path().to_string_lossy().into_owned(),
        reason: error.to_string(),
    })
}

fn configure_walker(
    root: &Path,
    exclude_patterns: &[String],
    exclude_matcher: &GlobSet,
    respect_gitignore: bool,
) -> Result<WalkBuilder, SourcesError> {
    let overrides = build_exclude_overrides(root, exclude_patterns)?;
    let exclude_for_prune = exclude_matcher.clone();
    let root_for_filter = root.to_path_buf();

    let mut builder = WalkBuilder::new(root);
    builder
        .git_ignore(respect_gitignore)
        .git_global(false)
        .git_exclude(respect_gitignore)
        .ignore(false)
        .follow_links(false)
        .hidden(false)
        .overrides(overrides)
        .filter_entry(move |entry| {
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(&root_for_filter) else {
                return true;
            };
            if entry
                .file_type()
                .is_some_and(|file_type| file_type.is_dir())
            {
                let rel_str = normalize_rel_path(rel);
                return !should_prune_dir(&rel_str, &exclude_for_prune);
            }
            true
        });
    Ok(builder)
}

/// Collect Python-related files under `root`.
pub fn collect_files(
    options: &CollectOptions<'_>,
) -> Result<(Vec<DiscoveredFile>, Vec<SourcesWarning>), SourcesError> {
    let root = options.root.to_path_buf();
    let project_matcher = options.project_matcher;
    let exclude_matcher = options.exclude_matcher;
    let production = options.production;
    let layout = options.layout;
    let gitignore = options.gitignore;

    let builder = configure_walker(
        &root,
        options.exclude_patterns,
        exclude_matcher,
        options.respect_gitignore,
    )?;

    let mut files = Vec::new();
    let mut warnings = Vec::new();

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

        if let Some(warning) = walk_entry_warning(&entry) {
            warnings.push(warning);
        }

        let path = entry.path();
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }

        let kind = match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("py") => FileKind::Python,
            Some(ext) if ext.eq_ignore_ascii_case("pyi") => FileKind::Stub,
            _ => continue,
        };

        let rel = path.strip_prefix(&root).map_err(|_| SourcesError::Io {
            path: root.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "failed to strip project root prefix",
            ),
        })?;
        let rel_str = normalize_rel_path(rel);

        if !project_matcher.is_match(&rel_str) || exclude_matcher.is_match(&rel_str) {
            continue;
        }

        if let Some(gitignore) = gitignore {
            let matched = gitignore.matched_path_or_any_parents(rel, false);
            if matched.is_ignore() {
                continue;
            }
        }

        let context = assign_file_context(&rel_str, layout);
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
pub fn validate_entries(root: &Path, entries: &[EntrySpec]) -> Vec<SourcesWarning> {
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
pub fn large_project_warning(file_count: usize) -> Option<SourcesWarning> {
    if file_count > LARGE_PROJECT_THRESHOLD {
        Some(SourcesWarning::LargeProject { file_count })
    } else {
        None
    }
}

/// Normalize a path to root-relative forward-slash form.
#[must_use]
pub fn normalize_rel_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if raw.contains('\\') {
        raw.replace('\\', "/")
    } else {
        raw.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::glob::{build_glob_set, effective_exclude};
    use crate::sources::types::ProjectLayout;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    #[test]
    fn collect_files_honors_project_and_exclude_globs() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        write_file(&root.join("src/acme/__init__.py"), "");
        write_file(&root.join("src/acme/module.py"), "");
        write_file(&root.join("tests/test_module.py"), "");
        write_file(&root.join("local/extra.py"), "");

        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: vec![
                "src/**/*.{py,pyi}".to_owned(),
                "tests/**/*.{py,pyi}".to_owned(),
            ],
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        let project = build_glob_set(&layout.inferred_globs).expect("project globs");
        let exclude_patterns = effective_exclude(&[]);
        let exclude = build_glob_set(&exclude_patterns).expect("exclude globs");

        let options = CollectOptions {
            root,
            project_matcher: &project,
            exclude_matcher: &exclude,
            exclude_patterns: &exclude_patterns,
            respect_gitignore: false,
            gitignore: None,
            production: false,
            layout: &layout,
        };
        let (files, _) = collect_files(&options).expect("collect");
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"src/acme/module.py"));
        assert!(paths.contains(&"tests/test_module.py"));
        assert!(!paths.contains(&"local/extra.py"));
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
            packages: vec!["acme".to_owned()],
            inferred_globs: vec![
                "src/**/*.{py,pyi}".to_owned(),
                "tests/**/*.{py,pyi}".to_owned(),
            ],
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        let project = build_glob_set(&layout.inferred_globs).expect("project globs");
        let exclude_patterns = effective_exclude(&[]);
        let exclude = build_glob_set(&exclude_patterns).expect("exclude globs");

        let options = CollectOptions {
            root,
            project_matcher: &project,
            exclude_matcher: &exclude,
            exclude_patterns: &exclude_patterns,
            respect_gitignore: false,
            gitignore: None,
            production: true,
            layout: &layout,
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
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            inferred_globs: vec!["**/*.{py,pyi}".to_owned()],
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        let project = build_glob_set(&layout.inferred_globs).expect("project globs");
        let exclude_patterns = effective_exclude(&[".venv/**".to_owned()]);
        let exclude = build_glob_set(&exclude_patterns).expect("exclude globs");

        let options = CollectOptions {
            root,
            project_matcher: &project,
            exclude_matcher: &exclude,
            exclude_patterns: &exclude_patterns,
            respect_gitignore: false,
            gitignore: None,
            production: false,
            layout: &layout,
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
            fn normalize_rel_path_strips_backslashes(raw in "\\PC{0,60}") {
                let normalized = normalize_rel_path(Path::new(&raw));
                prop_assert!(!normalized.contains('\\'));
            }

            #[test]
            fn normalize_rel_path_is_idempotent(raw in "\\PC{0,60}") {
                let once = normalize_rel_path(Path::new(&raw));
                prop_assert_eq!(normalize_rel_path(Path::new(&once)), once);
            }

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
