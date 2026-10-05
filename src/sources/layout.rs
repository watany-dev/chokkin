//! Project layout inference and default globs.

use std::fs;
use std::path::Path;

use crate::manifest::ProjectMetadata;

use super::types::{LayoutInfo, ProjectLayout};
use super::warnings::SourcesWarning;

const NON_PACKAGE_DIRS: &[&str] = &["tests", "scripts", "docs", "build", "dist", ".venv"];

/// `NON_PACKAGE_DIRS` that hold project source rather than build or
/// environment output, so an `__init__.py` makes them importable by name
/// (pytest puts the rootdir on `sys.path`).
const LOCAL_PACKAGE_DIRS: &[&str] = &["tests", "scripts", "docs"];

/// Infer project layout and default `project` globs (§3.1), plus a warning
/// when several flat-layout packages exist and metadata cannot pick one.
#[must_use]
pub fn infer_layout(
    root: &Path,
    metadata: &ProjectMetadata,
) -> (LayoutInfo, Option<SourcesWarning>) {
    let (mut layout, warning) = infer_distribution_layout(root, metadata);
    layout.local_packages = LOCAL_PACKAGE_DIRS
        .iter()
        .filter(|name| root.join(name).join("__init__.py").is_file())
        .map(|name| (*name).to_owned())
        .collect();
    (layout, warning)
}

fn infer_distribution_layout(
    root: &Path,
    metadata: &ProjectMetadata,
) -> (LayoutInfo, Option<SourcesWarning>) {
    let src_dir = root.join("src");
    if src_dir.is_dir() {
        let packages = packages_with_init(&src_dir, None);
        if !packages.is_empty() {
            let inferred_globs = default_globs(ProjectLayout::Src, &packages);
            let layout = LayoutInfo {
                layout: ProjectLayout::Src,
                packages,
                local_packages: Vec::new(),
                inferred_globs,
                members: Vec::new(),
            };
            return (layout, None);
        }
    }

    let flat_candidates = packages_with_init(root, Some(NON_PACKAGE_DIRS));
    if !flat_candidates.is_empty() {
        let (packages, warning) = resolve_flat_packages(flat_candidates, metadata);
        let inferred_globs = default_globs(ProjectLayout::Flat, &packages);
        let layout = LayoutInfo {
            layout: ProjectLayout::Flat,
            packages,
            local_packages: Vec::new(),
            inferred_globs,
            members: Vec::new(),
        };
        return (layout, warning);
    }

    let layout = LayoutInfo {
        layout: ProjectLayout::Unknown,
        packages: Vec::new(),
        local_packages: Vec::new(),
        inferred_globs: default_globs(ProjectLayout::Unknown, &[]),
        members: Vec::new(),
    };
    (layout, None)
}

/// Choose a flat-layout package when multiple candidates exist, warning when
/// metadata cannot disambiguate and the first candidate is taken.
fn resolve_flat_packages(
    candidates: Vec<String>,
    metadata: &ProjectMetadata,
) -> (Vec<String>, Option<SourcesWarning>) {
    let [first, _, ..] = candidates.as_slice() else {
        return (candidates, None);
    };
    let first = first.clone();

    if let Some(name) = &metadata.name {
        for candidate in normalized_project_names(name) {
            if candidates.contains(&candidate) {
                return (vec![candidate], None);
            }
        }
    }

    let warning = SourcesWarning::AmbiguousFlatLayout {
        candidates,
        chosen: first.clone(),
    };
    (vec![first], Some(warning))
}

/// Directory check from the type `read_dir` already holds; symlinks
/// still need a stat to keep links to directories included.
fn entry_is_dir(entry: &fs::DirEntry) -> bool {
    entry
        .file_type()
        .is_ok_and(|ft| ft.is_dir() || (ft.is_symlink() && entry.path().is_dir()))
}

fn packages_with_init(parent: &Path, skip_names: Option<&[&str]>) -> Vec<String> {
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
        if skip_names.is_some_and(|names| names.contains(&name)) {
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
        ProjectLayout::Src => vec!["src/**/*.{py,pyi,ipynb}".to_owned()],
        ProjectLayout::Flat => packages
            .iter()
            .map(|package| format!("{package}/**/*.{{py,pyi,ipynb}}"))
            .collect(),
        ProjectLayout::Unknown => vec!["**/*.{py,pyi,ipynb}".to_owned()],
    };
    globs.push("tests/**/*.{py,pyi,ipynb}".to_owned());
    globs.push("scripts/**/*.{py,pyi,ipynb}".to_owned());
    globs
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

/// Infer a dotted module name from a root-relative `.py` path.
///
/// This is the single source of truth for "which module is this file": the
/// `ModuleIndex` is keyed by it and relative imports are resolved against it,
/// so the two must never disagree. `None` means the file is not importable
/// under the inferred layout (outside `src/` and outside every package), and
/// callers treat imports from it as unresolved rather than inventing a name.
#[must_use]
pub fn path_to_module(path: &str, layout: &LayoutInfo) -> Option<String> {
    if let Some((member, rest)) = layout.member_for(path) {
        return path_to_module(rest, &member.layout)
            .or_else(|| namespace_module_name(rest, &member.layout));
    }
    let module_path = module_path(path)?;

    let distributed = match layout.layout {
        ProjectLayout::Src => src_module_name(module_path),
        ProjectLayout::Flat => package_module_name(module_path, &layout.packages),
        ProjectLayout::Unknown => src_module_name(module_path)
            .or_else(|| package_module_name(module_path, &layout.packages)),
    };
    distributed.or_else(|| package_module_name(module_path, &layout.local_packages))
}

/// A member with no regular package ships PEP 420 namespace packages
/// (`llama_index/` without `__init__.py`) straight from its root.
fn namespace_module_name(path: &str, layout: &LayoutInfo) -> Option<String> {
    let module_path = module_path(path)?;
    let (top, _) = module_path.split_once('/')?;
    if layout.layout != ProjectLayout::Unknown || NON_PACKAGE_DIRS.contains(&top) {
        return None;
    }
    Some(module_path.replace('/', "."))
}

fn module_path(path: &str) -> Option<&str> {
    let stem = path.strip_suffix(".py")?;
    Some(stem.strip_suffix("/__init__").unwrap_or(stem))
}

fn src_module_name(path: &str) -> Option<String> {
    path.strip_prefix("src/").map(|rest| rest.replace('/', "."))
}

fn package_module_name(path: &str, packages: &[String]) -> Option<String> {
    for package in packages {
        if path == *package {
            return Some(package.clone());
        }
        if let Some(suffix) = path.strip_prefix(&format!("{package}/")) {
            return Some(format!("{package}.{}", suffix.replace('/', ".")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ProjectMetadata;
    use crate::sources::MemberLayout;

    fn metadata(name: &str) -> ProjectMetadata {
        ProjectMetadata {
            name: Some(name.to_owned()),
            ..ProjectMetadata::default()
        }
    }

    #[test]
    fn path_to_module_src_layout() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
            members: Vec::new(),
        };
        assert_eq!(
            path_to_module("src/acme/api/routes.py", &layout),
            Some("acme.api.routes".to_owned())
        );
        assert_eq!(
            path_to_module("src/acme/__init__.py", &layout),
            Some("acme".to_owned())
        );
        assert_eq!(path_to_module("tests/unit/test_core.py", &layout), None);
    }

    #[test]
    fn path_to_module_unknown_layout_strips_src_prefix() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
            members: Vec::new(),
        };
        assert_eq!(
            path_to_module("src/acme/api/__init__.py", &layout),
            Some("acme.api".to_owned())
        );
        assert_eq!(path_to_module("tests/conftest.py", &layout), None);
    }

    #[test]
    fn path_to_module_flat_layout_requires_known_package() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Flat,
            packages: vec!["acme".to_owned()],
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
            members: Vec::new(),
        };
        assert_eq!(
            path_to_module("acme/core.py", &layout),
            Some("acme.core".to_owned())
        );
        assert_eq!(path_to_module("scripts/run.py", &layout), None);
    }

    #[test]
    fn path_to_module_resolves_inside_members() {
        let member = |path: &str, layout, packages: &[&str]| MemberLayout {
            path: path.to_owned(),
            layout: LayoutInfo {
                layout,
                packages: packages.iter().map(|&package| package.to_owned()).collect(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
        };
        let layout = LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
            members: vec![
                member("services/api", ProjectLayout::Src, &["api"]),
                member("llama-dev", ProjectLayout::Flat, &["llama_dev"]),
                member("llama-index-core", ProjectLayout::Unknown, &[]),
            ],
        };
        for (path, module) in [
            ("services/api/src/api/main.py", Some("api.main")),
            ("llama-dev/llama_dev/__init__.py", Some("llama_dev")),
            (
                "llama-index-core/llama_index/core/base.py",
                Some("llama_index.core.base"),
            ),
            ("services/api/tests/test_main.py", None),
            ("llama-index-core/tests/test_base.py", None),
            ("llama-index-core/setup.py", None),
        ] {
            assert_eq!(
                path_to_module(path, &layout),
                module.map(str::to_owned),
                "{path}"
            );
        }
    }

    #[test]
    fn infer_layout_indexes_root_tests_package_without_distributing_it() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        for dir in ["acme", "tests", "build"] {
            fs::create_dir_all(temp.path().join(dir)).expect("create dir");
            fs::write(temp.path().join(dir).join("__init__.py"), "").expect("write");
        }
        let (layout, warning) = infer_layout(temp.path(), &ProjectMetadata::default());
        assert_eq!(layout.layout, ProjectLayout::Flat);
        assert_eq!(layout.packages, vec!["acme".to_owned()]);
        assert_eq!(layout.local_packages, vec!["tests".to_owned()]);
        assert_eq!(warning, None);
        assert_eq!(
            path_to_module("tests/integration/client.py", &layout),
            Some("tests.integration.client".to_owned())
        );
        assert_eq!(
            path_to_module("tests/__init__.py", &layout),
            Some("tests".to_owned())
        );
        assert_eq!(path_to_module("build/lib.py", &layout), None);
        assert_eq!(path_to_module("testsuite/x.py", &layout), None);
    }

    #[test]
    fn default_globs_for_src_layout() {
        let globs = default_globs(ProjectLayout::Src, &["acme".to_owned()]);
        assert_eq!(
            globs,
            vec![
                "src/**/*.{py,pyi,ipynb}".to_owned(),
                "tests/**/*.{py,pyi,ipynb}".to_owned(),
                "scripts/**/*.{py,pyi,ipynb}".to_owned(),
            ]
        );
    }

    #[test]
    fn resolve_flat_prefers_metadata_name() {
        let candidates = vec!["acme".to_owned(), "other".to_owned()];
        let (packages, warning) = resolve_flat_packages(candidates, &metadata("acme-api"));
        assert_eq!(packages, vec!["acme".to_owned()]);
        assert_eq!(warning, None);
    }

    #[test]
    fn resolve_flat_falls_back_to_first_candidate_with_warning() {
        let candidates = vec!["alpha".to_owned(), "beta".to_owned()];
        let (packages, warning) =
            resolve_flat_packages(candidates.clone(), &ProjectMetadata::default());
        assert_eq!(packages, vec!["alpha".to_owned()]);
        assert_eq!(
            warning,
            Some(SourcesWarning::AmbiguousFlatLayout {
                candidates,
                chosen: "alpha".to_owned(),
            })
        );
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
                let (packages, warning) = resolve_flat_packages(candidates.clone(), &metadata);

                prop_assert!(packages.iter().all(|pkg| candidates.contains(pkg)));
                if candidates.len() <= 1 {
                    prop_assert_eq!(packages, candidates);
                    prop_assert!(warning.is_none());
                } else {
                    prop_assert_eq!(packages.len(), 1);
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
                let (packages, warning) = resolve_flat_packages(candidates, &metadata);
                prop_assert_eq!(packages, vec![target]);
                prop_assert!(warning.is_none());
            }

            #[test]
            fn default_globs_always_cover_tests_and_scripts(
                packages in candidate_names(),
            ) {
                for layout in [ProjectLayout::Src, ProjectLayout::Flat, ProjectLayout::Unknown] {
                    let globs = default_globs(layout, &packages);
                    let tests_glob = "tests/**/*.{py,pyi,ipynb}".to_owned();
                    let scripts_glob = "scripts/**/*.{py,pyi,ipynb}".to_owned();
                    prop_assert!(globs.contains(&tests_glob));
                    prop_assert!(globs.contains(&scripts_glob));
                }
            }
        }
    }
}
