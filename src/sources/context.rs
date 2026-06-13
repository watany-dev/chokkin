//! File context assignment (§10).

use super::types::{FileContext, LayoutInfo, ProjectLayout};

/// Assign a file context from a root-relative path and layout info.
///
/// `path` must already be in normalized forward-slash form, as produced
/// by [`super::walk::normalize_rel_path`].
#[must_use]
pub fn assign_file_context(path: &str, layout: &LayoutInfo) -> FileContext {
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
    if layout.layout == ProjectLayout::Flat {
        for package in &layout.packages {
            if path
                .strip_prefix(package.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
            {
                return FileContext::Runtime;
            }
        }
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
    (file_name.starts_with("test_")
        && std::path::Path::new(file_name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("py")))
        || file_name.ends_with("_test.py")
        || file_name.ends_with("_test.pyi")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::types::ProjectLayout;

    fn layout(layout: ProjectLayout, packages: &[&str]) -> LayoutInfo {
        LayoutInfo {
            layout,
            packages: packages.iter().map(|p| (*p).to_owned()).collect(),
            inferred_globs: Vec::new(),
        }
    }

    #[test]
    fn assigns_test_context_for_tests_tree() {
        let info = layout(ProjectLayout::Src, &["acme"]);
        assert_eq!(
            assign_file_context("tests/test_foo.py", &info),
            FileContext::Test
        );
    }

    #[test]
    fn assigns_test_context_for_test_module_pattern() {
        let info = layout(ProjectLayout::Src, &["acme"]);
        assert_eq!(
            assign_file_context("src/acme/test_utils.py", &info),
            FileContext::Test
        );
    }

    #[test]
    fn assigns_dev_context_for_scripts() {
        let info = layout(ProjectLayout::Src, &["acme"]);
        assert_eq!(
            assign_file_context("scripts/run.py", &info),
            FileContext::Dev
        );
    }

    #[test]
    fn assigns_runtime_for_src_tree() {
        let info = layout(ProjectLayout::Src, &["acme"]);
        assert_eq!(
            assign_file_context("src/acme/module.py", &info),
            FileContext::Runtime
        );
    }

    #[test]
    fn assigns_runtime_for_flat_package() {
        let info = layout(ProjectLayout::Flat, &["acme"]);
        assert_eq!(
            assign_file_context("acme/module.py", &info),
            FileContext::Runtime
        );
    }

    #[test]
    fn assigns_runtime_for_root_manage_py() {
        let info = layout(ProjectLayout::Unknown, &[]);
        assert_eq!(
            assign_file_context("manage.py", &info),
            FileContext::Runtime
        );
    }
}
