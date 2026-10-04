//! Project layout inference and default globs.

use std::fs;
use std::path::Path;

use globset::Glob;

use crate::manifest::{
    PackageFind, ProjectMetadata, UvSourceKind, UvToolSettings, WheelTargets,
    normalize_distribution_name,
};
use crate::path_util::join_rel;

use super::types::{LayoutInfo, ProjectLayout};
use super::warnings::SourcesWarning;

const NON_PACKAGE_DIRS: &[&str] = &[
    "tests",
    "scripts",
    "docs",
    "build",
    "dist",
    ".venv",
    "examples",
    "benchmarks",
];

/// `NON_PACKAGE_DIRS` that hold project source rather than build or
/// environment output, so an `__init__.py` makes them importable by name
/// (pytest puts the rootdir on `sys.path`).
const LOCAL_PACKAGE_DIRS: &[&str] = &["tests", "scripts", "docs"];

/// Source directory tried after `src/` and the repository root (#487).
const LIB_DIR: &str = "lib";

/// Infer project layout and default `project` globs (§3.1), plus a warning
/// when the package directory was guessed rather than declared.
///
/// Declarations win over directory names (#487): wheel targets first, then
/// `src/`, the root and `lib/` when a package there matches the project
/// name, then the `[tool.uv.sources]` path that provides this project, and
/// a guessed directory last.
#[must_use]
pub fn infer_layout(
    root: &Path,
    metadata: &ProjectMetadata,
    uv: &UvToolSettings,
) -> (LayoutInfo, Option<SourcesWarning>) {
    let (mut layout, warning) = declared_layout(root, metadata.wheel_targets.as_ref()).map_or_else(
        || {
            let heuristic = heuristic_layout(root, metadata);
            // A package named after the project beats a uv source that
            // only shares its prefix (`acme_api/` over `acme = { path }`).
            if heuristic.1.is_none() && heuristic.0.layout != ProjectLayout::Unknown {
                return heuristic;
            }
            uv_source_layout(root, metadata, uv).unwrap_or(heuristic)
        },
        |layout| (layout, None),
    );
    layout.local_packages = LOCAL_PACKAGE_DIRS
        .iter()
        .filter(|name| root.join(name).join("__init__.py").is_file())
        .map(|name| (*name).to_owned())
        .collect();
    (layout, warning)
}

fn layout_info(package_root: &str, packages: Vec<String>) -> LayoutInfo {
    let layout = if package_root.is_empty() {
        ProjectLayout::Flat
    } else {
        ProjectLayout::Src
    };
    LayoutInfo {
        layout,
        package_root: package_root.to_owned(),
        inferred_globs: default_globs(layout, package_root, &packages),
        packages,
        local_packages: Vec::new(),
    }
}

/// Layout from wheel target configuration, when every declared package
/// lives under one directory.
fn declared_layout(root: &Path, targets: Option<&WheelTargets>) -> Option<LayoutInfo> {
    let targets = targets?;
    let mut dirs: Vec<(String, String)> = targets
        .paths
        .iter()
        .filter(|path| !path.is_empty() && !path.contains(['*', '?', '[', '{']))
        .filter(|path| root.join(path).join("__init__.py").is_file())
        .filter_map(|path| top_level_package(root, path))
        .collect();
    for find in &targets.find {
        dirs.extend(found_packages(root, find));
    }
    dirs.sort();
    dirs.dedup();
    let (package_root, _) = dirs.first()?;
    if dirs.iter().any(|(other, _)| other != package_root) {
        return None;
    }
    let package_root = package_root.clone();
    let packages = dirs.into_iter().map(|(_, package)| package).collect();
    Some(layout_info(&package_root, packages))
}

/// Split a package directory into `(package_root, top-level package)`,
/// climbing out of parents that are packages themselves so `acme/sub` maps
/// to `acme`.
fn top_level_package(root: &Path, path: &str) -> Option<(String, String)> {
    let mut path = path;
    loop {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        if parent.is_empty() || !root.join(parent).join("__init__.py").is_file() {
            return Some((parent.to_owned(), name.to_owned()));
        }
        path = parent;
    }
}

/// Top-level packages setuptools `find` reports under a non-root `where`;
/// a root `where` is left to the heuristics, which skip non-package dirs.
fn found_packages(root: &Path, find: &PackageFind) -> Vec<(String, String)> {
    let matches = |patterns: &[String], name: &str| {
        patterns.iter().any(|pattern| {
            Glob::new(pattern).is_ok_and(|glob| glob.compile_matcher().is_match(name))
        })
    };
    find.where_dirs
        .iter()
        .filter(|base| !base.is_empty())
        .flat_map(|base| {
            packages_with_init(&root.join(base), false)
                .into_iter()
                .filter(|name| matches(&find.include, name) && !matches(&find.exclude, name))
                .map(move |name| (base.clone(), name))
        })
        .collect()
}

/// Layout of the in-tree `[tool.uv.sources]` path that provides this project
/// (streamlit's root `streamlit-dev` takes `streamlit` from `lib/`).
fn uv_source_layout(
    root: &Path,
    metadata: &ProjectMetadata,
    uv: &UvToolSettings,
) -> Option<(LayoutInfo, Option<SourcesWarning>)> {
    let names: Vec<String> = metadata
        .name
        .as_deref()
        .map(normalized_project_names)
        .unwrap_or_default()
        .iter()
        .map(|name| normalize_distribution_name(name))
        .collect();
    uv.sources.iter().find_map(|source| {
        let UvSourceKind::Path(path) = &source.kind else {
            return None;
        };
        let path = path.trim_start_matches("./").trim_end_matches('/');
        let inside_root = !path.is_empty()
            && !Path::new(path).is_absolute()
            && !path.split(['/', '\\']).any(|part| part == "..");
        if !inside_root || !names.contains(&normalize_distribution_name(&source.name)) {
            return None;
        }
        let member = ProjectMetadata {
            name: Some(source.name.clone()),
            ..ProjectMetadata::default()
        };
        let (inner, warning) = heuristic_layout(&root.join(path), &member);
        (inner.layout != ProjectLayout::Unknown).then(|| {
            let package_root = if inner.package_root.is_empty() {
                path.to_owned()
            } else {
                join_rel(path, &inner.package_root)
            };
            (layout_info(&package_root, inner.packages), warning)
        })
    })
}

/// Directory-name inference: `src/`, then root packages, then `lib/`, with a
/// package named after the project preferred over the first candidate.
fn heuristic_layout(
    root: &Path,
    metadata: &ProjectMetadata,
) -> (LayoutInfo, Option<SourcesWarning>) {
    let src_packages = packages_with_init(&root.join("src"), false);
    if !src_packages.is_empty() {
        return (layout_info("src", src_packages), None);
    }

    let candidates: Vec<(&str, Vec<String>)> = ["", LIB_DIR]
        .into_iter()
        .map(|base| (base, packages_with_init(&root.join(base), true)))
        .filter(|(_, packages)| !packages.is_empty())
        .collect();
    for (base, packages) in &candidates {
        if let Some(package) = project_package(packages, metadata) {
            return (layout_info(base, vec![package]), None);
        }
    }
    let Some((base, packages)) = candidates.into_iter().next() else {
        let layout = LayoutInfo {
            layout: ProjectLayout::Unknown,
            package_root: String::new(),
            packages: Vec::new(),
            local_packages: Vec::new(),
            inferred_globs: default_globs(ProjectLayout::Unknown, "", &[]),
        };
        return (layout, None);
    };
    let (package, warning) = guess_package(base, packages, metadata);
    (layout_info(base, vec![package]), warning)
}

/// The candidate named after the project, if any.
fn project_package(candidates: &[String], metadata: &ProjectMetadata) -> Option<String> {
    let name = metadata.name.as_deref()?;
    normalized_project_names(name)
        .into_iter()
        .find(|name| candidates.contains(name))
}

/// Take the first candidate when metadata cannot pick one, warning whenever
/// that is a guess: several candidates, or one unrelated to the project name.
fn guess_package(
    base: &str,
    candidates: Vec<String>,
    metadata: &ProjectMetadata,
) -> (String, Option<SourcesWarning>) {
    let dirs: Vec<String> = candidates
        .iter()
        .map(|package| join_rel(base, package))
        .collect();
    let chosen = dirs.first().cloned().unwrap_or_default();
    let warning = if dirs.len() > 1 {
        Some(SourcesWarning::AmbiguousPackageDir {
            candidates: dirs,
            chosen,
        })
    } else {
        metadata
            .name
            .clone()
            .map(|project| SourcesWarning::GuessedPackageDir { project, chosen })
    };
    (candidates.into_iter().next().unwrap_or_default(), warning)
}

/// Directory check from the type `read_dir` already holds; symlinks
/// still need a stat to keep links to directories included.
fn entry_is_dir(entry: &fs::DirEntry) -> bool {
    entry
        .file_type()
        .is_ok_and(|ft| ft.is_dir() || (ft.is_symlink() && entry.path().is_dir()))
}

fn is_non_package_dir(name: &str) -> bool {
    NON_PACKAGE_DIRS.contains(&name) || name.starts_with("e2e")
}

fn packages_with_init(parent: &Path, skip_non_packages: bool) -> Vec<String> {
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
        if skip_non_packages && is_non_package_dir(name) {
            continue;
        }
        if path.join("__init__.py").is_file() {
            packages.push(name.to_owned());
        }
    }
    packages.sort();
    packages
}

/// `src/` keeps its whole tree (top-level modules, namespace packages);
/// any other package root only contributes its packages, so `lib/tests`
/// next to `lib/streamlit` stays out of the runtime set.
fn default_globs(layout: ProjectLayout, package_root: &str, packages: &[String]) -> Vec<String> {
    let mut globs = match layout {
        ProjectLayout::Src if package_root == "src" => vec!["src/**/*.{py,pyi,ipynb}".to_owned()],
        ProjectLayout::Src | ProjectLayout::Flat => packages
            .iter()
            .map(|package| format!("{}/**/*.{{py,pyi,ipynb}}", join_rel(package_root, package)))
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
    let stem = path.strip_suffix(".py")?;
    let module_path = stem.strip_suffix("/__init__").unwrap_or(stem);

    let distributed = match layout.layout {
        ProjectLayout::Src => module_under(module_path, &layout.package_root),
        ProjectLayout::Flat => package_module_name(module_path, &layout.packages),
        ProjectLayout::Unknown => module_under(module_path, "src")
            .or_else(|| package_module_name(module_path, &layout.packages)),
    };
    distributed.or_else(|| package_module_name(module_path, &layout.local_packages))
}

fn module_under(path: &str, package_root: &str) -> Option<String> {
    path.strip_prefix(package_root)?
        .strip_prefix('/')
        .map(|rest| rest.replace('/', "."))
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
    use crate::manifest::{ProjectMetadata, UvSource};

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
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
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
            package_root: String::new(),
            packages: Vec::new(),
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
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
            package_root: String::new(),
            packages: vec!["acme".to_owned()],
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
        };
        assert_eq!(
            path_to_module("acme/core.py", &layout),
            Some("acme.core".to_owned())
        );
        assert_eq!(path_to_module("scripts/run.py", &layout), None);
    }

    #[test]
    fn infer_layout_indexes_root_tests_package_without_distributing_it() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        for dir in ["acme", "tests", "build"] {
            fs::create_dir_all(temp.path().join(dir)).expect("create dir");
            fs::write(temp.path().join(dir).join("__init__.py"), "").expect("write");
        }
        let (layout, warning) = infer_layout(
            temp.path(),
            &ProjectMetadata::default(),
            &UvToolSettings::default(),
        );
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
        let globs = default_globs(ProjectLayout::Src, "src", &["acme".to_owned()]);
        assert_eq!(
            globs,
            vec![
                "src/**/*.{py,pyi,ipynb}".to_owned(),
                "tests/**/*.{py,pyi,ipynb}".to_owned(),
                "scripts/**/*.{py,pyi,ipynb}".to_owned(),
            ]
        );
    }

    fn tree(dirs: &[&str]) -> tempfile::TempDir {
        let temp = tempfile::TempDir::new().expect("tempdir");
        for dir in dirs {
            fs::create_dir_all(temp.path().join(dir)).expect("create dir");
            fs::write(temp.path().join(dir).join("__init__.py"), "").expect("write");
        }
        temp
    }

    fn find_in(base: &str) -> WheelTargets {
        WheelTargets {
            source: "tool.setuptools".to_owned(),
            paths: Vec::new(),
            find: vec![PackageFind {
                where_dirs: vec![base.to_owned()],
                include: vec!["*".to_owned()],
                exclude: vec!["tests*".to_owned()],
                namespaces: false,
            }],
        }
    }

    #[test]
    fn setuptools_find_where_beats_root_directory_names() {
        let temp = tree(&["examples", "test", "lib/sqlalchemy", "lib/sqlalchemy/orm"]);
        let metadata = ProjectMetadata {
            wheel_targets: Some(find_in("lib")),
            ..metadata("SQLAlchemy")
        };
        let (layout, warning) = infer_layout(temp.path(), &metadata, &UvToolSettings::default());
        assert_eq!(layout.layout, ProjectLayout::Src);
        assert_eq!(layout.package_root, "lib");
        assert_eq!(layout.packages, vec!["sqlalchemy".to_owned()]);
        assert_eq!(warning, None);
        assert_eq!(
            layout.inferred_globs[0],
            "lib/sqlalchemy/**/*.{py,pyi,ipynb}"
        );
        assert_eq!(
            path_to_module("lib/sqlalchemy/orm/session.py", &layout),
            Some("sqlalchemy.orm.session".to_owned())
        );
        assert_eq!(path_to_module("examples/x.py", &layout), None);
    }

    #[test]
    fn declared_subpackage_maps_to_its_top_level_package() {
        let temp = tree(&["lib/acme", "lib/acme/sub"]);
        let metadata = ProjectMetadata {
            wheel_targets: Some(WheelTargets {
                source: "tool.setuptools".to_owned(),
                paths: vec!["lib/acme/sub".to_owned()],
                find: Vec::new(),
            }),
            ..ProjectMetadata::default()
        };
        let (layout, _) = infer_layout(temp.path(), &metadata, &UvToolSettings::default());
        assert_eq!(layout.package_root, "lib");
        assert_eq!(layout.packages, vec!["acme".to_owned()]);
    }

    #[test]
    fn declared_packages_under_different_roots_fall_back_to_heuristics() {
        let temp = tree(&["src/acme", "other/extra"]);
        let metadata = ProjectMetadata {
            wheel_targets: Some(WheelTargets {
                source: "tool.hatch.build".to_owned(),
                paths: vec!["src/acme".to_owned(), "other/extra".to_owned()],
                find: Vec::new(),
            }),
            ..ProjectMetadata::default()
        };
        let (layout, _) = infer_layout(temp.path(), &metadata, &UvToolSettings::default());
        assert_eq!(layout.package_root, "src");
        assert_eq!(layout.inferred_globs[0], "src/**/*.{py,pyi,ipynb}");
    }

    #[test]
    fn uv_path_source_providing_the_project_is_the_package_root() {
        let temp = tree(&["e2e_playwright", "lib/streamlit", "lib/tests"]);
        let uv = UvToolSettings {
            sources: vec![
                UvSource {
                    name: "helper".to_owned(),
                    kind: UvSourceKind::Path("vendor/helper".to_owned()),
                },
                UvSource {
                    name: "streamlit".to_owned(),
                    kind: UvSourceKind::Path("./lib".to_owned()),
                },
            ],
        };
        let (layout, warning) = infer_layout(temp.path(), &metadata("streamlit-dev"), &uv);
        assert_eq!(layout.package_root, "lib");
        assert_eq!(layout.packages, vec!["streamlit".to_owned()]);
        assert_eq!(warning, None);
        assert_eq!(
            path_to_module("lib/streamlit/runtime/app.py", &layout),
            Some("streamlit.runtime.app".to_owned())
        );
    }

    #[test]
    fn project_named_package_beats_uv_source_sharing_its_prefix() {
        let temp = tree(&["acme_api", "packages/acme"]);
        let uv = UvToolSettings {
            sources: vec![UvSource {
                name: "acme".to_owned(),
                kind: UvSourceKind::Path("packages/acme".to_owned()),
            }],
        };
        let (layout, warning) = infer_layout(temp.path(), &metadata("acme-api"), &uv);
        assert_eq!(layout.package_root, "");
        assert_eq!(layout.packages, vec!["acme_api".to_owned()]);
        assert_eq!(warning, None);
    }

    #[test]
    fn declared_directory_without_init_is_not_a_package() {
        let temp = tree(&["src/acme"]);
        let metadata = ProjectMetadata {
            wheel_targets: Some(WheelTargets {
                source: "tool.hatch".to_owned(),
                paths: vec!["src".to_owned()],
                find: Vec::new(),
            }),
            ..metadata("acme")
        };
        let (layout, _) = infer_layout(temp.path(), &metadata, &UvToolSettings::default());
        assert_eq!(layout.package_root, "src");
        assert_eq!(layout.packages, vec!["acme".to_owned()]);
        assert_eq!(
            path_to_module("src/acme/x.py", &layout),
            Some("acme.x".to_owned())
        );
    }

    #[test]
    fn uv_path_source_outside_root_is_ignored() {
        let temp = tree(&["acme"]);
        let uv = UvToolSettings {
            sources: vec![UvSource {
                name: "acme".to_owned(),
                kind: UvSourceKind::Path("../acme".to_owned()),
            }],
        };
        let (layout, _) = infer_layout(temp.path(), &metadata("acme"), &uv);
        assert_eq!(layout.layout, ProjectLayout::Flat);
        assert_eq!(layout.packages, vec!["acme".to_owned()]);
    }

    #[test]
    fn lib_package_named_after_project_beats_unrelated_root_package() {
        let temp = tree(&["tools", "lib/acme"]);
        let (layout, warning) =
            infer_layout(temp.path(), &metadata("acme"), &UvToolSettings::default());
        assert_eq!(layout.package_root, "lib");
        assert_eq!(layout.packages, vec!["acme".to_owned()]);
        assert_eq!(warning, None);
    }

    #[test]
    fn non_package_dirs_are_never_candidates() {
        let temp = tree(&["examples", "benchmarks", "e2e_playwright", "docs"]);
        let (layout, warning) =
            infer_layout(temp.path(), &metadata("acme"), &UvToolSettings::default());
        assert_eq!(layout.layout, ProjectLayout::Unknown);
        assert_eq!(warning, None);
    }

    #[test]
    fn single_unrelated_candidate_is_reported_as_a_guess() {
        let temp = tree(&["lib/other"]);
        let (layout, warning) =
            infer_layout(temp.path(), &metadata("acme"), &UvToolSettings::default());
        assert_eq!(layout.packages, vec!["other".to_owned()]);
        assert_eq!(
            warning,
            Some(SourcesWarning::GuessedPackageDir {
                project: "acme".to_owned(),
                chosen: "lib/other".to_owned(),
            })
        );
    }

    #[test]
    fn guess_prefers_first_candidate_and_warns_when_ambiguous() {
        let (package, warning) = guess_package(
            "",
            vec!["alpha".to_owned(), "beta".to_owned()],
            &ProjectMetadata::default(),
        );
        assert_eq!(package, "alpha");
        assert_eq!(
            warning,
            Some(SourcesWarning::AmbiguousPackageDir {
                candidates: vec!["alpha".to_owned(), "beta".to_owned()],
                chosen: "alpha".to_owned(),
            })
        );
    }

    #[test]
    fn guess_without_project_name_is_silent() {
        let (package, warning) =
            guess_package("", vec!["app".to_owned()], &ProjectMetadata::default());
        assert_eq!(package, "app");
        assert_eq!(warning, None);
    }

    #[test]
    fn project_package_prefers_metadata_name() {
        let candidates = vec!["acme".to_owned(), "other".to_owned()];
        assert_eq!(
            project_package(&candidates, &metadata("acme-api")),
            Some("acme".to_owned())
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
            fn guess_returns_a_candidate(
                candidates in prop::collection::vec("[a-z][a-z0-9_]{0,12}", 1..6),
                name in proptest::option::of("[A-Za-z][A-Za-z0-9_-]{0,16}"),
            ) {
                let metadata = ProjectMetadata {
                    name,
                    ..ProjectMetadata::default()
                };
                let (package, warning) = guess_package("lib", candidates.clone(), &metadata);
                prop_assert!(candidates.contains(&package));
                if candidates.len() > 1 {
                    let is_ambiguous =
                        matches!(warning, Some(SourcesWarning::AmbiguousPackageDir { .. }));
                    prop_assert!(is_ambiguous);
                }
            }

            #[test]
            fn project_package_finds_underscored_metadata_name(
                mut candidates in candidate_names(),
                target in "[a-z][a-z0-9_]{0,12}",
            ) {
                candidates.push(target.clone());
                let metadata = ProjectMetadata {
                    name: Some(target.replace('_', "-")),
                    ..ProjectMetadata::default()
                };
                prop_assert_eq!(project_package(&candidates, &metadata), Some(target));
            }

            #[test]
            fn default_globs_always_cover_tests_and_scripts(
                packages in candidate_names(),
            ) {
                for layout in [ProjectLayout::Src, ProjectLayout::Flat, ProjectLayout::Unknown] {
                    let globs = default_globs(layout, "src", &packages);
                    let tests_glob = "tests/**/*.{py,pyi,ipynb}".to_owned();
                    let scripts_glob = "scripts/**/*.{py,pyi,ipynb}".to_owned();
                    prop_assert!(globs.contains(&tests_glob));
                    prop_assert!(globs.contains(&scripts_glob));
                }
            }
        }
    }
}
