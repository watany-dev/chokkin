//! File context assignment (§10).

use super::types::FileContext;

/// Assign a file context from a root-relative path.
///
/// `path` must already be in normalized forward-slash form (as produced by the
/// source file walk).
#[must_use]
pub fn assign_file_context(path: &str) -> FileContext {
    if is_test_path(path) {
        return FileContext::Test;
    }
    if path.starts_with("docs/") {
        return FileContext::Docs;
    }
    if path.starts_with("scripts/") || path == "noxfile.py" {
        return FileContext::Dev;
    }
    FileContext::Runtime
}

/// `tests/` at any depth is test code (`pandas/tests/`), but only the root
/// `test/` is: a `test` package inside a distribution is shipped API
/// (`django.test`), as is `testing/` (`sqlalchemy.testing`).
fn is_test_path(path: &str) -> bool {
    if path.starts_with("test/") {
        return true;
    }
    let (dirs, file_name) = path.rsplit_once('/').unwrap_or(("", path));
    if dirs.split('/').any(|dir| dir == "tests") {
        return true;
    }
    if file_name == "conftest.py" {
        return true;
    }
    if file_name.starts_with("test_") && has_py_or_pyi_extension(file_name) {
        return true;
    }
    ends_with_ignore_ascii_case(file_name, "_test.py")
        || ends_with_ignore_ascii_case(file_name, "_test.pyi")
}

fn has_py_or_pyi_extension(file_name: &str) -> bool {
    std::path::Path::new(file_name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("py") || ext.eq_ignore_ascii_case("pyi"))
}

fn ends_with_ignore_ascii_case(value: &str, suffix: &str) -> bool {
    if value.len() < suffix.len() {
        return false;
    }
    let start = value.len() - suffix.len();
    value.is_char_boundary(start) && value[start..].eq_ignore_ascii_case(suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_test_context_for_tests_tree() {
        assert_eq!(assign_file_context("tests/test_foo.py"), FileContext::Test);
    }

    #[test]
    fn assigns_test_context_for_conftest() {
        assert_eq!(assign_file_context("tests/conftest.py"), FileContext::Test);
        assert_eq!(
            assign_file_context("src/acme/conftest.py"),
            FileContext::Test
        );
    }

    #[test]
    fn assigns_test_context_for_test_module_pattern() {
        assert_eq!(
            assign_file_context("src/acme/test_utils.py"),
            FileContext::Test
        );
    }

    #[test]
    fn assigns_test_context_for_test_pyi_stub() {
        assert_eq!(
            assign_file_context("src/acme/test_utils.pyi"),
            FileContext::Test
        );
    }

    #[test]
    fn assigns_test_context_for_uppercase_py_extension() {
        assert_eq!(
            assign_file_context("src/acme/test_utils.PY"),
            FileContext::Test
        );
        assert_eq!(
            assign_file_context("src/acme/module_test.PYI"),
            FileContext::Test
        );
    }

    #[test]
    fn assigns_test_context_for_in_package_tests_tree() {
        assert_eq!(
            assign_file_context("pandas/tests/frame/common.py"),
            FileContext::Test
        );
        assert_eq!(
            assign_file_context("src/acme/tests/__init__.py"),
            FileContext::Test
        );
    }

    #[test]
    fn singular_test_dir_is_test_context_only_at_root() {
        assert_eq!(
            assign_file_context("test/base/helpers.py"),
            FileContext::Test
        );
        assert_eq!(
            assign_file_context("django/test/client.py"),
            FileContext::Runtime
        );
        assert_eq!(
            assign_file_context("lib/sqlalchemy/testing/fixtures.py"),
            FileContext::Runtime
        );
    }

    #[test]
    fn tests_must_be_a_whole_directory_name() {
        assert_eq!(
            assign_file_context("acme/mytests/util.py"),
            FileContext::Runtime
        );
        assert_eq!(assign_file_context("acme/tests.py"), FileContext::Runtime);
    }

    #[test]
    fn assigns_dev_context_for_scripts() {
        assert_eq!(assign_file_context("scripts/run.py"), FileContext::Dev);
    }

    #[test]
    fn assigns_runtime_for_src_tree() {
        assert_eq!(
            assign_file_context("src/acme/module.py"),
            FileContext::Runtime
        );
    }

    #[test]
    fn assigns_runtime_for_flat_package() {
        assert_eq!(assign_file_context("acme/module.py"), FileContext::Runtime);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn assign_file_context_never_panics(path in "\\PC{0,80}") {
                let _ = assign_file_context(&path);
            }

            #[test]
            fn tests_tree_is_always_test_context(rest in "[a-z0-9_/]{0,30}") {
                prop_assert_eq!(
                    assign_file_context(&format!("tests/{rest}.py")),
                    FileContext::Test
                );
            }

            #[test]
            fn test_prefix_files_are_test_context_anywhere(
                dir in "[a-z][a-z0-9_/]{0,20}",
                name in "[a-z][a-z0-9_]{0,12}",
            ) {
                prop_assert_eq!(
                    assign_file_context(&format!("{dir}/test_{name}.py")),
                    FileContext::Test
                );
                prop_assert_eq!(
                    assign_file_context(&format!("{dir}/{name}_test.py")),
                    FileContext::Test
                );
            }

            #[test]
            fn src_tree_non_test_files_are_runtime(name in "[a-z][a-z0-9_]{0,12}") {
                prop_assume!(
                    name != "conftest" && !name.starts_with("test_") && !name.ends_with("_test")
                );
                prop_assert_eq!(
                    assign_file_context(&format!("src/pkg/{name}.py")),
                    FileContext::Runtime
                );
            }
        }

        const DIRS: &[&str] = &[
            "tests", "test", "Tests", "testing", "mytests", "docs", "scripts", "src", "acme", "lib",
        ];

        const FILES: &[&str] = &[
            "conftest.py",
            "noxfile.py",
            "test_x.py",
            "test_x.pyi",
            "test_x.PY",
            "Test_x.py",
            "test_x.txt",
            "test_",
            "x_test.py",
            "x_TEST.Pyi",
            "x_test.pyc",
            "_test.py",
            "tests.py",
            "mod.py",
            "é_test.py",
        ];

        fn path_strategy() -> impl Strategy<Value = (Vec<&'static str>, &'static str)> {
            (
                prop::collection::vec(prop::sample::select(DIRS), 0..4),
                prop::sample::select(FILES),
            )
        }

        /// Spec §10 file-context rules, written from the directory list.
        fn model(dirs: &[&str], file: &str) -> FileContext {
            let lower = file.to_ascii_lowercase();
            let test_file = file == "conftest.py"
                || (file.starts_with("test_")
                    && matches!(lower.rsplit_once('.'), Some((_, "py" | "pyi"))))
                || lower.ends_with("_test.py")
                || lower.ends_with("_test.pyi");
            if dirs.first() == Some(&"test") || dirs.contains(&"tests") || test_file {
                FileContext::Test
            } else if dirs.first() == Some(&"docs") {
                FileContext::Docs
            } else if dirs.first() == Some(&"scripts") || (dirs.is_empty() && file == "noxfile.py")
            {
                FileContext::Dev
            } else {
                FileContext::Runtime
            }
        }

        fn join(dirs: &[&str], file: &str) -> String {
            dirs.iter()
                .copied()
                .chain([file])
                .collect::<Vec<_>>()
                .join("/")
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn assign_file_context_matches_reference_model((dirs, file) in path_strategy()) {
                let path = join(&dirs, file);
                prop_assert_eq!(assign_file_context(&path), model(&dirs, file), "{}", path);
            }

            /// Moving a file under a `tests/` directory, at any depth, makes
            /// it test code; nesting a test file deeper never makes it
            /// runtime code unless it only lived in the root `test/`.
            #[test]
            fn tests_dir_and_nesting_are_monotone(
                (dirs, file) in path_strategy(),
                at in 0usize..4,
                outer in prop::sample::select(&["acme", "src", "lib"][..]),
            ) {
                let mut nested = dirs.clone();
                nested.insert(at.min(dirs.len()), "tests");
                prop_assert_eq!(assign_file_context(&join(&nested, file)), FileContext::Test);

                let path = join(&dirs, file);
                let mut deeper = vec![outer];
                deeper.extend(&dirs);
                if assign_file_context(&path) == FileContext::Test && dirs.first() != Some(&"test") {
                    prop_assert_eq!(
                        assign_file_context(&join(&deeper, file)),
                        FileContext::Test,
                        "{}", path
                    );
                }
            }
        }
    }
}
