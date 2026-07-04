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
    if path.starts_with("src/") {
        return FileContext::Runtime;
    }

    FileContext::Runtime
}

fn is_test_path(path: &str) -> bool {
    if path.starts_with("tests/") {
        return true;
    }
    let Some(file_name) = path.rsplit('/').next() else {
        return false;
    };
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

    #[test]
    fn assigns_runtime_for_root_manage_py() {
        assert_eq!(assign_file_context("manage.py"), FileContext::Runtime);
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
            fn assign_file_context_is_deterministic(path in "\\PC{0,80}") {
                prop_assert_eq!(
                    assign_file_context(&path),
                    assign_file_context(&path)
                );
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
                prop_assume!(!name.starts_with("test_") && !name.ends_with("_test"));
                prop_assert_eq!(
                    assign_file_context(&format!("src/pkg/{name}.py")),
                    FileContext::Runtime
                );
            }
        }
    }
}
