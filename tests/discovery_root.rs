//! Integration tests for project root discovery.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use yokei::{DiscoveryError, RootMarker, discover_project_root};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/discovery")
        .join(name)
}

fn assert_discovered(start: &Path, expected_root: &Path, expected_marker: RootMarker) {
    let result = discover_project_root(start).expect("discover project root");
    assert_eq!(result.marker, expected_marker);
    assert_eq!(result.path, fs_canonicalize(expected_root));
}

fn fs_canonicalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[test]
fn discovers_pyproject_at_start() {
    let root = fixture("pyproject_only");
    assert_discovered(&root, &root, RootMarker::PyProjectToml);
}

#[test]
fn discovers_pyproject_from_nested_dir() {
    let root = fixture("nested_src");
    let start = root.join("src/pkg");
    assert_discovered(&start, &root, RootMarker::PyProjectToml);
}

#[test]
fn prefers_pyproject_over_requirements() {
    let root = fixture("multi_marker");
    assert_discovered(&root, &root, RootMarker::PyProjectToml);
}

#[test]
fn discovers_uv_lock_marker() {
    let root = fixture("uv_lock_only");
    assert_discovered(&root, &root, RootMarker::UvLock);
}

#[test]
fn discovers_setup_cfg_marker() {
    let root = fixture("setup_cfg_only");
    assert_discovered(&root, &root, RootMarker::SetupCfg);
}

#[test]
fn discovers_setup_py_marker() {
    let root = fixture("setup_py_only");
    assert_discovered(&root, &root, RootMarker::SetupPy);
}

#[test]
fn discovers_requirements_txt_marker() {
    let root = fixture("requirements_only");
    assert_discovered(&root, &root, RootMarker::RequirementsTxt);
}

#[test]
fn discovers_git_when_no_manifest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    std::fs::create_dir_all(root.join(".git")).expect("create .git");
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").expect("write HEAD");

    assert_discovered(root, root, RootMarker::Git);
}

#[test]
fn returns_not_found_for_empty_tree() {
    let temp = tempfile::tempdir().expect("tempdir");

    let error = discover_project_root(temp.path()).expect_err("expected not found");
    assert!(matches!(error, DiscoveryError::NotFound { .. }));
}

#[test]
fn returns_invalid_start_for_file() {
    let root = fixture("pyproject_only");
    let file = root.join("pyproject.toml");

    let error = discover_project_root(&file).expect_err("expected invalid start");
    assert!(matches!(error, DiscoveryError::InvalidStart { .. }));
}

#[test]
fn returns_invalid_start_for_missing_path() {
    let missing = fixture("no_marker").join("does-not-exist");

    let error = discover_project_root(&missing).expect_err("expected invalid start");
    assert!(matches!(error, DiscoveryError::InvalidStart { .. }));
}

#[test]
fn monorepo_subdir_uses_nearest_root() {
    let sub = fixture("monorepo_subdir/root/pkg/sub");
    assert_discovered(&sub, &sub, RootMarker::PyProjectToml);
}

#[test]
fn start_path_preserved_in_result() {
    let relative = Path::new("tests/fixtures/discovery/nested_src/src/pkg");

    let result = discover_project_root(relative).expect("discover from relative path");
    assert_eq!(result.start, relative);
    assert_eq!(result.marker, RootMarker::PyProjectToml);
}
