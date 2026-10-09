//! File context assignment (§10).

use super::types::{DiscoveredSources, FileContext, LayoutInfo};

/// Assign a file context from a root-relative path.
///
/// `path` must already be in normalized forward-slash form (as produced by the
/// source file walk).
#[must_use]
pub(crate) fn assign_file_context(path: &str) -> FileContext {
    if is_test_path(path) {
        return FileContext::Test;
    }
    if path.starts_with("docs/") {
        return FileContext::Docs;
    }
    if path.starts_with("scripts/") || path == "noxfile.py" || is_example_notebook(path) {
        return FileContext::Dev;
    }
    FileContext::Runtime
}

/// A notebook under `examples/` demonstrates the package; no wheel ships it
/// (#681).
fn is_example_notebook(path: &str) -> bool {
    path.starts_with("examples/")
        && std::path::Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ipynb"))
}

/// [`assign_file_context`] that also knows `<member>/test/` is test,
/// `<member>/docs/` docs and `<member>/examples/*.ipynb` dev, for files
/// `--production` dropped from the inventory.
#[must_use]
pub(crate) fn assign_layout_file_context(path: &str, layout: &LayoutInfo) -> FileContext {
    match assign_file_context(path) {
        FileContext::Runtime => member_context(path, layout).unwrap_or(FileContext::Runtime),
        context => context,
    }
}

/// Give `<member>/test/`, `<member>/docs/` and `<member>/examples/*.ipynb`
/// files the context root `test/`, `docs/` and `examples/*.ipynb` get.
/// Members are detected after the walk, so this runs once they are known
/// (#612, #571, #681).
pub(crate) fn apply_member_context(sources: &mut DiscoveredSources, production: bool) {
    let layout = &sources.layout;
    for file in &mut sources.files {
        if file.context == FileContext::Runtime
            && let Some(context) = member_context(&file.path, layout)
        {
            file.context = context;
        }
    }
    if production {
        sources
            .files
            .retain(|file| file.context.is_included_in_production());
    }
}

fn member_context(path: &str, layout: &LayoutInfo) -> Option<FileContext> {
    let (_, rest) = layout.member_for(path)?;
    if rest.starts_with("test/") {
        Some(FileContext::Test)
    } else if rest.starts_with("docs/") {
        Some(FileContext::Docs)
    } else if is_example_notebook(rest) {
        Some(FileContext::Dev)
    } else {
        None
    }
}

/// `tests/` at any depth is test code (`pandas/tests/`), but only the root
/// (and, via [`apply_member_context`], a member's) `test/` is: a `test`
/// package inside a distribution is shipped API (`django.test`), as is
/// `testing/` (`sqlalchemy.testing`).
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
    // pytest's `test_*.py` / `*_test.py` are case-sensitive (#570); only the
    // extension is not, for case-insensitive file systems.
    file_name.rsplit_once('.').is_some_and(|(stem, ext)| {
        (ext.eq_ignore_ascii_case("py") || ext.eq_ignore_ascii_case("pyi"))
            && (stem.starts_with("test_") || stem.ends_with("_test"))
    })
}

/// Whether `path` is input data under a test tree (`tests/data/case.py`,
/// `test/fixtures/demo/setup.py`): tests read these as files rather than
/// import them, so being unreachable is expected (#593).
#[must_use]
pub(crate) fn is_test_data_path(path: &str) -> bool {
    let Some((dirs, _)) = path.rsplit_once('/') else {
        return false;
    };
    let mut dirs = dirs.split('/');
    let in_test_tree = if path.starts_with("test/") {
        dirs.next().is_some()
    } else {
        dirs.any(|dir| dir == "tests")
    };
    in_test_tree && dirs.any(|dir| matches!(dir, "data" | "fixtures" | "testdata" | "test_data"))
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
    fn test_prefix_and_suffix_are_case_sensitive() {
        for path in [
            "acme/Test_foo.py",
            "acme/TEST_foo.py",
            "acme/foo_TEST.py",
            "acme/foo_Test.pyi",
        ] {
            assert_eq!(assign_file_context(path), FileContext::Runtime, "{path}");
        }
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
    fn data_dirs_under_a_test_tree_are_test_data() {
        for path in [
            "tests/data/cases/allow_empty_first_line.py",
            "tests/fixtures/projects/demo/demo.py",
            "test/mitmproxy/data/addonscripts/addon.py",
            "pkg/tests/testdata/sample.py",
            "tests/unit/test_data/input.py",
        ] {
            assert!(is_test_data_path(path), "{path}");
        }
    }

    #[test]
    fn data_dirs_outside_a_test_tree_are_not_test_data() {
        for path in [
            "acme/data/loader.py",
            "fixtures/tests/helper.py",
            "tests/test_data.py",
            "tests/conftest.py",
            "django/test/data/x.py",
            "data.py",
        ] {
            assert!(!is_test_data_path(path), "{path}");
        }
    }

    /// A root with the single member `providers/google`, holding `paths`
    /// after [`apply_member_context`].
    fn member_sources(paths: &[&str], production: bool) -> (DiscoveredSources, LayoutInfo) {
        use crate::discovery::{ProjectRoot, RootMarker};
        use crate::sources::{DiscoveredFile, FileKind, MemberLayout, ProjectLayout};

        let layout = || LayoutInfo {
            layout: ProjectLayout::Unknown,
            package_root: String::new(),
            packages: Vec::new(),
            local_packages: Vec::new(),
            inferred_globs: Vec::new(),
            members: Vec::new(),
        };
        let mut root_layout = layout();
        root_layout.members = vec![MemberLayout {
            path: "providers/google".to_owned(),
            layout: layout(),
        }];
        let mut sources = DiscoveredSources {
            root: ProjectRoot {
                path: std::path::PathBuf::from("/project"),
                marker: RootMarker::PyProjectToml,
            },
            layout: root_layout.clone(),
            effective_globs: Vec::new(),
            files: paths
                .iter()
                .map(|path| DiscoveredFile {
                    path: (*path).to_owned(),
                    kind: FileKind::Python,
                    context: assign_file_context(path),
                })
                .collect(),
            warnings: Vec::new(),
        };
        apply_member_context(&mut sources, production);
        (sources, root_layout)
    }

    fn contexts(sources: &DiscoveredSources) -> Vec<FileContext> {
        sources.files.iter().map(|file| file.context).collect()
    }

    fn kept(sources: &DiscoveredSources) -> Vec<&str> {
        sources
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect()
    }

    #[test]
    fn member_test_and_docs_files_get_root_contexts() {
        let paths = [
            "providers/google/docs/conf.py",
            "providers/google/docs/tests/test_conf.py",
            "providers/google/src/docs/helper.py",
            "providers/other/docs/conf.py",
            "providers/google/test/helpers.py",
            "providers/google/src/test/client.py",
        ];

        let (all, root_layout) = member_sources(&paths, false);
        assert_eq!(
            contexts(&all),
            [
                FileContext::Docs,
                FileContext::Test,
                FileContext::Runtime,
                FileContext::Runtime,
                FileContext::Test,
                FileContext::Runtime
            ]
        );

        let (production, _) = member_sources(&paths, true);
        assert_eq!(
            kept(&production),
            [
                "providers/google/src/docs/helper.py",
                "providers/other/docs/conf.py",
                "providers/google/src/test/client.py"
            ]
        );
        assert_eq!(
            assign_layout_file_context(paths[0], &root_layout),
            FileContext::Docs
        );
        assert_eq!(
            assign_layout_file_context(paths[1], &root_layout),
            FileContext::Test
        );
        assert_eq!(
            assign_layout_file_context(paths[4], &root_layout),
            FileContext::Test
        );
    }

    #[test]
    fn member_example_notebooks_get_root_context() {
        let paths = [
            "providers/google/examples/demo.ipynb",
            "providers/google/examples/demo.py",
            "providers/google/src/examples/demo.ipynb",
        ];

        let (all, root_layout) = member_sources(&paths, false);
        assert_eq!(
            contexts(&all),
            [FileContext::Dev, FileContext::Runtime, FileContext::Runtime]
        );

        let (production, _) = member_sources(&paths, true);
        assert_eq!(
            kept(&production),
            [
                "providers/google/examples/demo.py",
                "providers/google/src/examples/demo.ipynb"
            ]
        );
        assert_eq!(
            assign_layout_file_context(paths[0], &root_layout),
            FileContext::Dev
        );
    }

    #[test]
    fn assigns_dev_context_for_scripts() {
        assert_eq!(assign_file_context("scripts/run.py"), FileContext::Dev);
    }

    #[test]
    fn assigns_dev_context_only_for_example_notebooks() {
        assert_eq!(assign_file_context("examples/demo.ipynb"), FileContext::Dev);
        assert_eq!(
            assign_file_context("examples/demo.py"),
            FileContext::Runtime
        );
        assert_eq!(
            assign_file_context("src/acme/examples/demo.ipynb"),
            FileContext::Runtime
        );
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
            let test_file = file == "conftest.py"
                || file.rsplit_once('.').is_some_and(|(stem, ext)| {
                    matches!(ext.to_ascii_lowercase().as_str(), "py" | "pyi")
                        && (stem.starts_with("test_") || stem.ends_with("_test"))
                });
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
