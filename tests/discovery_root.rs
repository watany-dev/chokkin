//! Integration tests for project root discovery.

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::path::{Path, PathBuf};

use chokkin::internals::{DiscoveryError, RootMarker, discover_project_root};

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

fn has_project_marker(dir: &Path) -> bool {
    [
        "pyproject.toml",
        "uv.lock",
        "setup.cfg",
        "setup.py",
        "requirements.txt",
        ".git",
    ]
    .iter()
    .any(|marker| dir.join(marker).exists())
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
fn discovers_git_gitfile_when_no_manifest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    std::fs::write(root.join(".git"), "gitdir: /path/to/real/.git\n").expect("write gitfile");

    assert_discovered(root, root, RootMarker::Git);
}

/// A marker that cannot be stat'ed for any reason but absence is an I/O
/// error, not a missing marker. A symlink loop fails the same way for root,
/// which permission bits would not stop (#1136).
#[cfg(unix)]
#[test]
fn returns_io_error_for_unreadable_marker() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();
    std::os::unix::fs::symlink("pyproject.toml", dir.join("pyproject.toml")).expect("symlink");

    let error = discover_project_root(dir).expect_err("expected io error");

    assert!(
        matches!(&error, DiscoveryError::Io { path, .. } if path.ends_with("pyproject.toml")),
        "{error:?}"
    );
}

/// No marker up to the file system root is `NotFound`. When the temp
/// directory sits under a marker, the walk must stop at the nearest one
/// instead, so the test asserts that rather than passing vacuously (#1136).
#[test]
fn returns_not_found_for_empty_tree() {
    let temp = tempfile::tempdir().expect("tempdir");
    let start = std::fs::canonicalize(temp.path()).expect("canonical tempdir");

    if let Some(nearest) = start
        .ancestors()
        .skip(1)
        .find(|dir| has_project_marker(dir))
    {
        let root = discover_project_root(&start).expect("marker above the tempdir");
        assert_eq!(root.path, nearest);
        return;
    }

    let error = discover_project_root(&start).expect_err("expected not found");
    assert!(matches!(error, DiscoveryError::NotFound { .. }));
}

/// A marker counts only under its own spelling, so a case-insensitive file
/// system finds no root where Linux finds none (#1163).
#[test]
fn ignores_markers_spelled_in_another_case() {
    let temp = tempfile::tempdir().expect("tempdir");
    for name in [
        "PyProject.toml",
        "UV.lock",
        "Setup.cfg",
        "Setup.py",
        "Requirements.txt",
    ] {
        std::fs::write(temp.path().join(name), "").expect("write marker");
    }
    std::fs::create_dir(temp.path().join(".GIT")).expect("create dir");

    let start = std::fs::canonicalize(temp.path()).expect("canonical tempdir");
    if let Some(nearest) = start
        .ancestors()
        .skip(1)
        .find(|dir| has_project_marker(dir))
    {
        let root = discover_project_root(&start).expect("marker above the tempdir");
        assert_eq!(root.path, nearest);
        return;
    }

    let error = discover_project_root(&start).expect_err("expected not found");
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
fn relative_start_path_is_resolved() {
    let relative = Path::new("tests/fixtures/discovery/nested_src/src/pkg");
    assert_discovered(relative, &fixture("nested_src"), RootMarker::PyProjectToml);
}
