//! Project layout inference and default globs.

use std::fs;
use std::path::Path;

use crate::manifest::ProjectMetadata;

use super::types::{LayoutInfo, ProjectLayout};
use super::warnings::SourcesWarning;

const NON_PACKAGE_DIRS: &[&str] = &["tests", "scripts", "docs", "build", "dist", ".venv"];

/// Infer project layout and default `project` globs (§3.1).
#[must_use]
pub fn infer_layout(root: &Path, metadata: &ProjectMetadata) -> LayoutInfo {
    let src_dir = root.join("src");
    if src_dir.is_dir() {
        let packages = packages_with_init(&src_dir);
        if !packages.is_empty() {
            let inferred_globs = default_globs(ProjectLayout::Src, &packages);
            return LayoutInfo {
                layout: ProjectLayout::Src,
                packages,
                inferred_globs,
            };
        }
    }

    let flat_candidates = flat_package_candidates(root);
    if !flat_candidates.is_empty() {
        let packages = resolve_flat_packages(&flat_candidates, metadata);
        let inferred_globs = default_globs(ProjectLayout::Flat, &packages);
        return LayoutInfo {
            layout: ProjectLayout::Flat,
            packages,
            inferred_globs,
        };
    }

    LayoutInfo {
        layout: ProjectLayout::Unknown,
        packages: Vec::new(),
        inferred_globs: default_globs(ProjectLayout::Unknown, &[]),
    }
}

/// Choose a flat-layout package when multiple candidates exist.
#[must_use]
pub fn resolve_flat_packages(candidates: &[String], metadata: &ProjectMetadata) -> Vec<String> {
    if candidates.len() <= 1 {
        return candidates.to_vec();
    }

    if let Some(name) = &metadata.name {
        for candidate in normalized_project_names(name) {
            if candidates.iter().any(|pkg| pkg == &candidate) {
                return vec![candidate];
            }
        }
    }

    vec![candidates[0].clone()]
}

/// Directory check from the type `read_dir` already holds; symlinks
/// still need a stat to keep links to directories included.
fn entry_is_dir(entry: &fs::DirEntry) -> bool {
    entry
        .file_type()
        .is_ok_and(|ft| ft.is_dir() || (ft.is_symlink() && entry.path().is_dir()))
}

fn packages_with_init(parent: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };

    let mut packages = Vec::new();
    for entry in entries.flatten() {
        if !entry_is_dir(&entry) {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if path.join("__init__.py").is_file() {
            packages.push(name.to_owned());
        }
    }
    packages.sort();
    packages
}

fn flat_package_candidates(root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    let mut packages = Vec::new();
    for entry in entries.flatten() {
        if !entry_is_dir(&entry) {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if NON_PACKAGE_DIRS.contains(&name) {
            continue;
        }
        if path.join("__init__.py").is_file() {
            packages.push(name.to_owned());
        }
    }
    packages.sort();
    packages
}

fn default_globs(layout: ProjectLayout, packages: &[String]) -> Vec<String> {
    let mut globs = match layout {
        ProjectLayout::Src => vec!["src/**/*.{py,pyi}".to_owned()],
        ProjectLayout::Flat => packages
            .iter()
            .map(|package| format!("{package}/**/*.{{py,pyi}}"))
            .collect(),
        ProjectLayout::Unknown => vec!["**/*.{py,pyi}".to_owned()],
    };
    globs.push("tests/**/*.{py,pyi}".to_owned());
    globs.push("scripts/**/*.{py,pyi}".to_owned());
    globs
}

/// Build layout-related warnings such as ambiguous flat packages.
#[must_use]
pub fn layout_warnings(root: &Path, layout: &LayoutInfo) -> Vec<SourcesWarning> {
    if layout.layout != ProjectLayout::Flat {
        return Vec::new();
    }

    let candidates = flat_package_candidates(root);
    if candidates.len() <= 1 {
        return Vec::new();
    }

    let Some(chosen) = layout.packages.first() else {
        return Vec::new();
    };

    vec![SourcesWarning::AmbiguousFlatLayout {
        candidates,
        chosen: chosen.clone(),
    }]
}

fn normalized_project_names(name: &str) -> Vec<String> {
    let underscored = name.replace('-', "_");
    let mut names = vec![underscored.clone(), name.to_owned()];
    if let Some(base) = underscored.split('_').next()
        && base != underscored
    {
        names.push(base.to_owned());
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ProjectMetadata;

    fn metadata(name: &str) -> ProjectMetadata {
        ProjectMetadata {
            name: Some(name.to_owned()),
            ..ProjectMetadata::default()
        }
    }

    #[test]
    fn default_globs_for_src_layout() {
        let globs = default_globs(ProjectLayout::Src, &["acme".to_owned()]);
        assert_eq!(
            globs,
            vec![
                "src/**/*.{py,pyi}".to_owned(),
                "tests/**/*.{py,pyi}".to_owned(),
                "scripts/**/*.{py,pyi}".to_owned(),
            ]
        );
    }

    #[test]
    fn resolve_flat_prefers_metadata_name() {
        let candidates = vec!["acme".to_owned(), "other".to_owned()];
        let chosen = resolve_flat_packages(&candidates, &metadata("acme-api"));
        assert_eq!(chosen, vec!["acme".to_owned()]);
    }

    #[test]
    fn resolve_flat_falls_back_to_first_candidate() {
        let candidates = vec!["alpha".to_owned(), "beta".to_owned()];
        let chosen = resolve_flat_packages(&candidates, &ProjectMetadata::default());
        assert_eq!(chosen, vec!["alpha".to_owned()]);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn candidate_names() -> impl Strategy<Value = Vec<String>> {
            prop::collection::vec("[a-z][a-z0-9_]{0,12}", 0..6)
        }

        proptest! {
            #[test]
            fn resolve_flat_returns_subset_of_candidates(
                candidates in candidate_names(),
                name in proptest::option::of("[A-Za-z][A-Za-z0-9_-]{0,16}"),
            ) {
                let metadata = ProjectMetadata {
                    name,
                    ..ProjectMetadata::default()
                };
                let resolved = resolve_flat_packages(&candidates, &metadata);

                prop_assert!(resolved.iter().all(|pkg| candidates.contains(pkg)));
                if candidates.len() <= 1 {
                    prop_assert_eq!(resolved, candidates);
                } else {
                    prop_assert_eq!(resolved.len(), 1);
                }
            }

            #[test]
            fn resolve_flat_prefers_underscored_metadata_name(
                mut candidates in candidate_names(),
                target in "[a-z][a-z0-9_]{0,12}",
            ) {
                candidates.push(target.clone());
                candidates.sort();
                candidates.dedup();

                let metadata = ProjectMetadata {
                    name: Some(target.replace('_', "-")),
                    ..ProjectMetadata::default()
                };
                let resolved = resolve_flat_packages(&candidates, &metadata);
                prop_assert_eq!(resolved, vec![target]);
            }

            #[test]
            fn default_globs_always_cover_tests_and_scripts(
                packages in candidate_names(),
            ) {
                for layout in [ProjectLayout::Src, ProjectLayout::Flat, ProjectLayout::Unknown] {
                    let globs = default_globs(layout, &packages);
                    let tests_glob = "tests/**/*.{py,pyi}".to_owned();
                    let scripts_glob = "scripts/**/*.{py,pyi}".to_owned();
                    prop_assert!(globs.contains(&tests_glob));
                    prop_assert!(globs.contains(&scripts_glob));
                }
            }
        }
    }
}
