//! Directory names that mean something other than the distributed package
//! (§10): test suites, tooling, and build or environment output.

/// Root directories holding the test suite. Only [`NESTED_TEST_DIR`] marks
/// tests below the root: a `test` package inside a distribution is shipped
/// API (`django.test`).
const TEST_DIRS: &[&str] = &["tests", "test"];

/// The test directory that is test code at any depth (`pandas/tests/`).
pub(super) const NESTED_TEST_DIR: &str = "tests";

/// Root directories that hold project source rather than build or
/// environment output, so an `__init__.py` makes them importable by name
/// (pytest puts the rootdir on `sys.path`).
pub(super) fn local_package_dirs() -> impl Iterator<Item = &'static str> {
    TEST_DIRS.iter().copied().chain(["scripts", "docs"])
}

/// Root directories every layout scans besides its package: the local
/// package directories and `examples/`, whose files are entries (§8,
/// #1023). An `examples/` package keeps no module name: its scripts run as
/// files (sqlalchemy's `examples/`).
pub(super) fn default_scanned_dirs() -> impl Iterator<Item = &'static str> {
    local_package_dirs().chain(["examples"])
}

/// Directories never taken as the distributed package, nor as a namespace
/// package of a member.
pub(super) fn is_non_package_dir(name: &str) -> bool {
    local_package_dirs().any(|dir| dir == name)
        || ["build", "dist", ".venv", "examples", "benchmarks"].contains(&name)
        || name.starts_with("e2e")
}

pub(super) fn in_root_test_dir(path: &str) -> bool {
    path.split_once('/')
        .is_some_and(|(top, _)| TEST_DIRS.contains(&top))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_package_dirs_cover_every_scanned_dir() {
        for dir in default_scanned_dirs() {
            assert!(is_non_package_dir(dir), "{dir}");
        }
        for dir in [
            "build",
            "dist",
            ".venv",
            "examples",
            "benchmarks",
            "e2e",
            "e2e_tests",
        ] {
            assert!(is_non_package_dir(dir), "{dir}");
        }
        for dir in ["acme", "testing", "lib", "src"] {
            assert!(!is_non_package_dir(dir), "{dir}");
        }
    }

    #[test]
    fn root_test_dir_is_the_first_component_only() {
        assert!(in_root_test_dir("tests/test_a.py"));
        assert!(in_root_test_dir("test/base/x.py"));
        assert!(!in_root_test_dir("acme/test/x.py"));
        assert!(!in_root_test_dir("testing/x.py"));
        assert!(!in_root_test_dir("tests"));
    }
}
