//! Directory walking with glob, exclude, and gitignore filters.

use std::path::Path;

use globset::GlobSet;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use walkdir::WalkDir;

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
    /// Optional gitignore matcher.
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

/// Collect Python-related files under `root`.
pub fn collect_files(options: &CollectOptions<'_>) -> Result<Vec<DiscoveredFile>, SourcesError> {
    let CollectOptions {
        root,
        project_matcher,
        exclude_matcher,
        gitignore,
        production,
        layout,
    } = options;

    let mut files = Vec::new();

    for entry in WalkDir::new(*root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let rel = path.strip_prefix(*root).map_err(|_| SourcesError::Io {
            path: (*root).to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "failed to strip project root prefix",
            ),
        })?;
        let rel_str = normalize_rel_path(rel);

        let kind = match path.extension().and_then(|ext| ext.to_str()) {
            Some("py") => FileKind::Python,
            Some("pyi") => FileKind::Stub,
            _ => continue,
        };

        if !project_matcher.is_match(&rel_str) || exclude_matcher.is_match(&rel_str) {
            continue;
        }

        if let Some(gitignore) = *gitignore {
            let matched = gitignore.matched_path_or_any_parents(rel, false);
            if matched.is_ignore() {
                continue;
            }
        }

        let context = assign_file_context(&rel_str, layout);
        if *production && !context.is_included_in_production() {
            continue;
        }

        files.push(DiscoveredFile {
            path: rel_str,
            kind,
            context,
        });
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
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
    path.to_string_lossy().replace('\\', "/")
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
            inferred_globs: vec!["src/**/*.py".to_owned(), "tests/**/*.py".to_owned()],
        };
        let project = build_glob_set(&layout.inferred_globs).expect("project globs");
        let exclude = build_glob_set(&effective_exclude(&[])).expect("exclude globs");

        let options = CollectOptions {
            root,
            project_matcher: &project,
            exclude_matcher: &exclude,
            gitignore: None,
            production: false,
            layout: &layout,
        };
        let files = collect_files(&options).expect("collect");
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
            inferred_globs: vec!["src/**/*.py".to_owned(), "tests/**/*.py".to_owned()],
        };
        let project = build_glob_set(&layout.inferred_globs).expect("project globs");
        let exclude = build_glob_set(&effective_exclude(&[])).expect("exclude globs");

        let options = CollectOptions {
            root,
            project_matcher: &project,
            exclude_matcher: &exclude,
            gitignore: None,
            production: true,
            layout: &layout,
        };
        let files = collect_files(&options).expect("collect");
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"src/acme/module.py"));
        assert!(!paths.contains(&"tests/test_module.py"));
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
}
