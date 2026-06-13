//! Property-based tests for source file discovery (public API surface).
//!
//! Generates random project trees and checks discovery invariants: sorted
//! unique output, root-relative `/` paths, exclusion guarantees, production
//! subset behavior, and determinism.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::Path;

use proptest::prelude::*;
use yokei::{
    ConfigSources, DiscoveredSources, LoadedConfig, ProjectRoot, RootMarker, default_config,
    discover_sources, extract_manifest,
};

fn project_root_at(path: &Path) -> ProjectRoot {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    ProjectRoot {
        path: canonical,
        marker: RootMarker::PyProjectToml,
        start: path.to_path_buf(),
    }
}

fn default_loaded_config(root: &ProjectRoot) -> LoadedConfig {
    LoadedConfig {
        root: root.clone(),
        effective: default_config(),
        sources: ConfigSources {
            used_defaults: true,
            dot_yokei_toml: None,
            yokei_toml: None,
            pyproject_tool_yokei: false,
        },
        uv_workspace: None,
    }
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write file");
}

/// Root-relative file paths: 1-3 short lowercase segments plus an extension.
fn tree_paths() -> impl Strategy<Value = Vec<String>> {
    let segment = "[a-z][a-z0-9_]{0,6}";
    let extension = prop_oneof![Just("py"), Just("pyi"), Just("txt"), Just("md")];
    prop::collection::btree_set(
        (prop::collection::vec(segment, 1..4), extension)
            .prop_map(|(segments, ext)| format!("{}.{ext}", segments.join("/"))),
        0..15,
    )
    .prop_map(|set| set.into_iter().collect())
}

fn discover_tree(root_dir: &Path, production: bool) -> DiscoveredSources {
    let root = project_root_at(root_dir);
    let mut config = default_loaded_config(&root);
    config.effective.production = production;
    let manifest = extract_manifest(&root, &config).expect("manifest extraction");
    discover_sources(&root, &config, &manifest).expect("source discovery")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn discovery_invariants_hold_for_random_trees(paths in tree_paths()) {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("pyproject.toml"), "[project]\nname = \"acme\"\n");
        for path in &paths {
            write_file(&temp.path().join(path), "");
        }

        let sources = discover_tree(temp.path(), false);

        // Sorted strictly ascending implies uniqueness.
        for window in sources.files.windows(2) {
            prop_assert!(window[0].path < window[1].path);
        }
        for file in &sources.files {
            prop_assert!(
                Path::new(&file.path).extension().is_some_and(|ext| ext == "py" || ext == "pyi"),
                "non-Python file discovered: {}",
                file.path
            );
            prop_assert!(!file.path.starts_with('/'));
            prop_assert!(!file.path.contains('\\'));
            prop_assert!(!file.path.contains("__pycache__"));
        }
    }

    #[test]
    fn discovery_is_deterministic(paths in tree_paths()) {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("pyproject.toml"), "[project]\nname = \"acme\"\n");
        for path in &paths {
            write_file(&temp.path().join(path), "");
        }

        prop_assert_eq!(discover_tree(temp.path(), false), discover_tree(temp.path(), false));
    }

    #[test]
    fn production_files_are_subset_of_full_discovery(paths in tree_paths()) {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("pyproject.toml"), "[project]\nname = \"acme\"\n");
        for path in &paths {
            write_file(&temp.path().join(path), "");
        }

        let full: Vec<String> = discover_tree(temp.path(), false)
            .files
            .into_iter()
            .map(|file| file.path)
            .collect();
        let production = discover_tree(temp.path(), true);
        for file in &production.files {
            prop_assert!(full.contains(&file.path));
            prop_assert!(file.context.is_included_in_production());
        }
    }

    #[test]
    fn pycache_files_are_never_discovered(
        package in "[a-z][a-z0-9_]{0,8}",
        module in "[a-z][a-z0-9_]{0,8}",
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("pyproject.toml"), "[project]\nname = \"acme\"\n");
        write_file(&temp.path().join(format!("src/{package}/__init__.py")), "");
        write_file(&temp.path().join(format!("src/{package}/{module}.py")), "");
        write_file(
            &temp.path().join(format!("src/{package}/__pycache__/{module}.py")),
            "",
        );

        let sources = discover_tree(temp.path(), false);
        let module_path = format!("src/{package}/{module}.py");
        prop_assert!(sources.files.iter().any(|file| file.path == module_path));
        prop_assert!(
            sources
                .files
                .iter()
                .all(|file| !file.path.contains("__pycache__"))
        );
    }

    #[test]
    fn gitignored_files_are_skipped(
        package in "[a-z][a-z0-9_]{0,8}",
        kept in "[a-z][a-z0-9_]{0,8}",
        ignored in "[a-z][a-z0-9_]{0,8}",
    ) {
        prop_assume!(kept != ignored);
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("pyproject.toml"), "[project]\nname = \"acme\"\n");
        write_file(&temp.path().join(format!("src/{package}/__init__.py")), "");
        write_file(&temp.path().join(format!("src/{package}/{kept}.py")), "");
        write_file(&temp.path().join(format!("src/{package}/{ignored}.py")), "");
        write_file(&temp.path().join(".gitignore"), &format!("{ignored}.py\n"));

        let sources = discover_tree(temp.path(), false);
        let kept_path = format!("src/{package}/{kept}.py");
        let ignored_path = format!("src/{package}/{ignored}.py");
        let paths: Vec<&str> = sources.files.iter().map(|file| file.path.as_str()).collect();
        prop_assert!(paths.contains(&kept_path.as_str()));
        prop_assert!(!paths.contains(&ignored_path.as_str()));
    }

    #[test]
    fn explicit_project_globs_limit_discovery(paths in tree_paths()) {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(&temp.path().join("pyproject.toml"), "[project]\nname = \"acme\"\n");
        for path in &paths {
            write_file(&temp.path().join(path), "");
        }

        let root = project_root_at(temp.path());
        let mut config = default_loaded_config(&root);
        config.effective.project = vec!["app/**/*.py".to_owned()];
        let manifest = extract_manifest(&root, &config).expect("manifest extraction");
        let sources = discover_sources(&root, &config, &manifest).expect("source discovery");

        prop_assert_eq!(&sources.effective_globs, &config.effective.project);
        for file in &sources.files {
            prop_assert!(file.path.starts_with("app/"), "outside glob: {}", file.path);
        }
    }
}
