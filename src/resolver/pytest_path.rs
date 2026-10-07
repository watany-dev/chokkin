//! pytest's default `--import-mode=prepend` (#360).
//!
//! Before importing a test module or `conftest.py`, pytest inserts its
//! basedir into `sys.path`: the file's own directory, or the parent of its
//! top-level package when the directory has an `__init__.py`. Files next to a
//! test or conftest therefore import under top-level names (`from lifecycle
//! import X`), which neither the layout nor the module index knows about.
//!
//! Two more `sys.path` entries live here (#589): the project root for tests,
//! which reaches root directories the source globs never walk (`docs_src/`,
//! a src-layout project's `dummyserver/`), and, as a fallback, a non-test
//! file's own directory, which `python path/to/script.py` puts first.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::plugins::{PytestImportSettings, pytest_import_settings};
use crate::sources::{DiscoveredSources, FileContext, FileKind};

/// Extra import directories that apply to test-context files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PytestImportPaths {
    files: HashSet<String>,
    /// Every directory holding a discovered file, at any depth.
    dirs: HashSet<String>,
    /// Test-context file → the directories on `sys.path` while it runs, in
    /// lookup order (`""` is the project root).
    search: HashMap<String, Vec<String>>,
    /// Root-level modules and regular packages on disk, discovered or not.
    root_modules: HashSet<String>,
    /// Root-level directories without `__init__.py`: PEP 420 portions, which
    /// lose to a regular package anywhere else on `sys.path`.
    root_namespaces: HashSet<String>,
}

impl PytestImportPaths {
    #[must_use]
    pub(crate) fn build(sources: &DiscoveredSources) -> Self {
        Self::with_settings(sources, &pytest_import_settings(&sources.root.path))
            .with_root_entries(&sources.root.path)
    }

    #[must_use]
    pub(crate) fn with_settings(
        sources: &DiscoveredSources,
        settings: &PytestImportSettings,
    ) -> Self {
        let mut files = HashSet::new();
        let mut dirs = HashSet::new();
        for file in &sources.files {
            if !matches!(file.kind, FileKind::Python | FileKind::Stub) {
                continue;
            }
            let mut dir = parent_dir(&file.path);
            while !dir.is_empty() && dirs.insert(dir.to_owned()) {
                dir = parent_dir(dir);
            }
            files.insert(file.path.clone());
        }

        let pythonpath: Vec<String> = settings
            .pythonpath
            .iter()
            .map(|entry| normalize_dir(entry))
            .collect();
        let conftest_basedirs: Vec<(&str, String)> = if settings.importlib {
            Vec::new()
        } else {
            sources
                .files
                .iter()
                .filter(|file| file_name(&file.path) == "conftest.py")
                .map(|file| (parent_dir(&file.path), basedir(&file.path, &files)))
                .collect()
        };

        let mut search = HashMap::new();
        for file in &sources.files {
            if file.context != FileContext::Test {
                continue;
            }
            let mut file_dirs = Vec::new();
            if !settings.importlib {
                file_dirs.push(basedir(&file.path, &files));
                let dir = parent_dir(&file.path);
                for (conftest_dir, conftest_base) in &conftest_basedirs {
                    if is_same_or_under(dir, conftest_dir) {
                        file_dirs.push(conftest_base.clone());
                    }
                }
            }
            file_dirs.extend(pythonpath.iter().cloned());
            // `python -m pytest` and IDE runners start from the root.
            file_dirs.push(String::new());
            let mut seen = HashSet::new();
            file_dirs.retain(|dir| seen.insert(dir.clone()));
            search.insert(file.path.clone(), file_dirs);
        }

        Self {
            files,
            dirs,
            search,
            root_modules: HashSet::new(),
            root_namespaces: HashSet::new(),
        }
    }

    /// Record the root's top-level entries, which the walk may have skipped.
    #[must_use]
    pub(crate) fn with_root_entries(mut self, root: &Path) -> Self {
        let Ok(entries) = std::fs::read_dir(root) else {
            return self;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if is_identifier(&name) {
                    if path.join("__init__.py").is_file() {
                        self.root_modules.insert(name);
                    } else {
                        self.root_namespaces.insert(name);
                    }
                }
            } else if let Some(stem) = name
                .strip_suffix(".py")
                .or_else(|| name.strip_suffix(".pyi"))
                .filter(|stem| is_identifier(stem))
            {
                self.root_modules.insert(stem.to_owned());
            }
        }
        self
    }

    /// Whether top-level module `root` is a local file or directory on the
    /// `sys.path` pytest gives `file`.
    #[must_use]
    pub(crate) fn provides_root(&self, file: &str, root: &str) -> bool {
        self.search_dirs(file).any(|dir| {
            self.has_module(dir, root) || (dir.is_empty() && self.root_modules.contains(root))
        })
    }

    /// Whether `root` is local to `file` only when nothing else provides it:
    /// a root namespace directory for a test, or a module beside a script
    /// (`scripts/ci/prek/common_prek_utils.py`).
    #[must_use]
    pub(crate) fn provides_fallback(&self, file: &str, root: &str) -> bool {
        if self.search.contains_key(file) {
            self.root_namespaces.contains(root)
        } else {
            self.files.contains(file) && self.has_module(parent_dir(file), root)
        }
    }

    /// The module beside non-test `file` that `module` names, if any.
    #[must_use]
    pub(crate) fn resolve_sibling(&self, file: &str, module: &str) -> Option<&str> {
        if self.search.contains_key(file) || !self.files.contains(file) {
            return None;
        }
        self.find_module(parent_dir(file), module)
    }

    fn has_module(&self, dir: &str, root: &str) -> bool {
        let base = join(dir, root);
        self.dirs.contains(&base)
            || self.files.contains(&format!("{base}.py"))
            || self.files.contains(&format!("{base}.pyi"))
    }

    fn find_module(&self, dir: &str, module: &str) -> Option<&str> {
        let base = join(dir, &module.replace('.', "/"));
        [format!("{base}.py"), format!("{base}/__init__.py")]
            .into_iter()
            .find_map(|candidate| self.files.get(&candidate).map(String::as_str))
    }

    #[must_use]
    pub(crate) fn resolve(&self, file: &str, module: &str) -> Option<&str> {
        self.search_dirs(file)
            .find_map(|dir| self.find_module(dir, module))
    }

    fn search_dirs(&self, file: &str) -> impl Iterator<Item = &str> {
        self.search
            .get(file)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }
}

/// The directory pytest's prepend mode inserts for `path`: its directory, or
/// the parent of the outermost package that directory belongs to.
fn basedir(path: &str, files: &HashSet<String>) -> String {
    let mut dir = parent_dir(path);
    while !dir.is_empty() && files.contains(&join(dir, "__init__.py")) {
        dir = parent_dir(dir);
    }
    dir.to_owned()
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn join(dir: &str, rest: &str) -> String {
    if dir.is_empty() {
        rest.to_owned()
    } else {
        format!("{dir}/{rest}")
    }
}

fn is_same_or_under(dir: &str, ancestor: &str) -> bool {
    ancestor.is_empty()
        || dir == ancestor
        || dir
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `./src/`, `src` and `src/` all name `src`; `.` names the root.
fn normalize_dir(entry: &str) -> String {
    let entry = entry.replace('\\', "/");
    let mut entry = entry.as_str();
    while let Some(rest) = entry.strip_prefix("./") {
        entry = rest;
    }
    let entry = entry.trim_end_matches('/');
    if entry == "." {
        String::new()
    } else {
        entry.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::sources::{DiscoveredFile, LayoutInfo, ProjectLayout, assign_file_context};

    fn sources(paths: &[&str]) -> DiscoveredSources {
        DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                package_root: "src".to_owned(),
                packages: vec!["acme".to_owned()],
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
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
        }
    }

    const E2E: [&str; 6] = [
        "src/acme/__init__.py",
        "tests/e2e/conftest.py",
        "tests/e2e/lifecycle.py",
        "tests/e2e/management/__init__.py",
        "tests/e2e/access/test_access.py",
        "tests/unit/test_core.py",
    ];

    #[test]
    fn conftest_directory_is_on_the_path_of_tests_below_it() {
        let paths =
            PytestImportPaths::with_settings(&sources(&E2E), &PytestImportSettings::default());
        let test = "tests/e2e/access/test_access.py";
        assert_eq!(
            paths.resolve(test, "lifecycle"),
            Some("tests/e2e/lifecycle.py")
        );
        assert_eq!(
            paths.resolve(test, "management"),
            Some("tests/e2e/management/__init__.py")
        );
        assert!(paths.provides_root(test, "management"));
        assert!(paths.provides_root(test, "access"));
        assert!(!paths.provides_root(test, "requests"));
        // A sibling tree has no conftest there, so `lifecycle` is not on its path.
        assert_eq!(paths.resolve("tests/unit/test_core.py", "lifecycle"), None);
        // Runtime code is never affected.
        assert_eq!(paths.resolve("src/acme/__init__.py", "lifecycle"), None);
    }

    #[test]
    fn package_test_files_use_the_parent_of_the_top_level_package() {
        let files = [
            "tests/pkg/__init__.py",
            "tests/pkg/sub/__init__.py",
            "tests/pkg/sub/test_x.py",
            "tests/pkg/helpers.py",
        ];
        let paths =
            PytestImportPaths::with_settings(&sources(&files), &PytestImportSettings::default());
        let test = "tests/pkg/sub/test_x.py";
        assert_eq!(
            paths.resolve(test, "pkg.helpers"),
            Some("tests/pkg/helpers.py")
        );
        assert_eq!(paths.resolve(test, "helpers"), None);
    }

    #[test]
    fn importlib_mode_keeps_only_pythonpath() {
        let settings = PytestImportSettings {
            pythonpath: vec!["./tests/e2e/".to_owned()],
            importlib: true,
            ..PytestImportSettings::default()
        };
        let paths = PytestImportPaths::with_settings(&sources(&E2E), &settings);
        assert_eq!(
            paths.resolve("tests/unit/test_core.py", "lifecycle"),
            Some("tests/e2e/lifecycle.py")
        );
        let importlib_only = PytestImportSettings {
            pythonpath: Vec::new(),
            importlib: true,
            ..PytestImportSettings::default()
        };
        let paths = PytestImportPaths::with_settings(&sources(&E2E), &importlib_only);
        assert_eq!(
            paths.resolve("tests/e2e/access/test_access.py", "lifecycle"),
            None
        );
    }

    #[test]
    fn only_conftest_files_put_their_directory_on_the_path() {
        let files = ["tests/helpers.py", "tests/unit/test_core.py"];
        let paths =
            PytestImportPaths::with_settings(&sources(&files), &PytestImportSettings::default());
        assert_eq!(paths.resolve("tests/unit/test_core.py", "helpers"), None);
    }

    #[test]
    fn root_conftest_puts_the_root_on_the_path_of_every_test() {
        let files = ["conftest.py", "rootmod.py", "tests/unit/test_core.py"];
        let paths =
            PytestImportPaths::with_settings(&sources(&files), &PytestImportSettings::default());
        assert_eq!(
            paths.resolve("tests/unit/test_core.py", "rootmod"),
            Some("rootmod.py")
        );
    }

    #[test]
    fn tests_reach_root_entries_the_walk_skipped() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        for dir in ["docs_src/tutorial001", "dummyserver"] {
            std::fs::create_dir_all(temp.path().join(dir)).expect("mkdir");
        }
        std::fs::write(temp.path().join("dummyserver/__init__.py"), "").expect("write");
        std::fs::write(temp.path().join("rootmod.py"), "").expect("write");
        let files = ["src/acme/__init__.py", "tests/unit/test_core.py"];
        let paths =
            PytestImportPaths::with_settings(&sources(&files), &PytestImportSettings::default())
                .with_root_entries(temp.path());
        let test = "tests/unit/test_core.py";
        assert!(paths.provides_root(test, "dummyserver"));
        assert!(paths.provides_root(test, "rootmod"));
        // A namespace portion loses to a regular package anywhere on the path.
        assert!(!paths.provides_root(test, "docs_src"));
        assert!(paths.provides_fallback(test, "docs_src"));
        // Runtime code does not run with the root on its path.
        assert!(!paths.provides_root("src/acme/__init__.py", "dummyserver"));
        assert!(!paths.provides_fallback("src/acme/__init__.py", "docs_src"));
    }

    #[test]
    fn non_test_files_fall_back_to_modules_beside_them() {
        let files = [
            "src/acme/__init__.py",
            "scripts/ci/prek/__init__.py",
            "scripts/ci/prek/check.py",
            "scripts/ci/prek/common_utils.py",
            "tests/e2e/test_x.py",
            "tests/e2e/helpers.py",
            "tests/pkg/__init__.py",
            "tests/pkg/test_y.py",
            "tests/pkg/helpers.py",
        ];
        let paths =
            PytestImportPaths::with_settings(&sources(&files), &PytestImportSettings::default());
        let script = "scripts/ci/prek/check.py";
        assert!(paths.provides_fallback(script, "common_utils"));
        assert_eq!(
            paths.resolve_sibling(script, "common_utils"),
            Some("scripts/ci/prek/common_utils.py")
        );
        assert!(!paths.provides_root(script, "common_utils"));
        // Tests follow pytest's rules, not the script fallback.
        assert!(paths.provides_root("tests/e2e/test_x.py", "helpers"));
        assert!(!paths.provides_fallback("tests/pkg/test_y.py", "helpers"));
        assert_eq!(
            paths.resolve_sibling("tests/pkg/test_y.py", "helpers"),
            None
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn segments() -> impl Strategy<Value = Vec<String>> {
            prop::collection::vec("[a-z][a-z0-9_]{0,3}", 0..4)
        }

        proptest! {
            #[test]
            fn normalize_dir_strips_dot_slash_and_trailing_slashes(
                dir in segments(),
                leading in 0usize..3,
                trailing in 0usize..3,
                backslashes in any::<bool>(),
            ) {
                let clean = dir.join("/");
                let mut entry = format!("{}{clean}{}", "./".repeat(leading), "/".repeat(trailing));
                if backslashes {
                    entry = entry.replace('/', "\\");
                }
                prop_assert_eq!(normalize_dir(&entry), clean.as_str());
                prop_assert_eq!(normalize_dir(&clean), clean);
            }

            #[test]
            fn is_same_or_under_compares_whole_segments(
                dir in segments(),
                ancestor in segments(),
            ) {
                // Reference model: `ancestor`'s segments prefix `dir`'s, so
                // `src/ab` is not under `src/a`.
                prop_assert_eq!(
                    is_same_or_under(&dir.join("/"), &ancestor.join("/")),
                    dir.starts_with(&ancestor)
                );
            }

            #[test]
            fn join_then_parent_dir_roundtrips(dir in segments(), name in "[a-z][a-z0-9_]{0,3}") {
                let dir = dir.join("/");
                let path = join(&dir, &name);
                prop_assert_eq!(parent_dir(&path), dir.as_str());
                prop_assert_eq!(file_name(&path), name.as_str());
            }
        }
    }
}
