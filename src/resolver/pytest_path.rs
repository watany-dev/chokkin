//! pytest's default `--import-mode=prepend` (#360).
//!
//! Before importing a test module or `conftest.py`, pytest inserts its
//! basedir into `sys.path`: the file's own directory, or the parent of its
//! top-level package when the directory has an `__init__.py`. Files next to a
//! test or conftest therefore import under top-level names (`from lifecycle
//! import X`), which neither the layout nor the module index knows about.

use std::collections::{HashMap, HashSet};

use crate::plugins::{PytestImportSettings, pytest_import_settings};
use crate::sources::{DiscoveredSources, FileContext, FileKind};

/// Extra import directories that apply to test-context files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PytestImportPaths {
    /// Discovered `.py` / `.pyi` paths.
    files: HashSet<String>,
    /// Every directory holding a discovered file, at any depth.
    dirs: HashSet<String>,
    /// Test-context file → the directories on `sys.path` while it runs, in
    /// lookup order (`""` is the project root).
    search: HashMap<String, Vec<String>>,
}

impl PytestImportPaths {
    /// Build from discovered sources and the pytest config under their root.
    #[must_use]
    pub fn build(sources: &DiscoveredSources) -> Self {
        Self::with_settings(sources, &pytest_import_settings(&sources.root.path))
    }

    /// Build from discovered sources and already-read pytest settings.
    #[must_use]
    pub fn with_settings(sources: &DiscoveredSources, settings: &PytestImportSettings) -> Self {
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
            let mut seen = HashSet::new();
            file_dirs.retain(|dir| seen.insert(dir.clone()));
            if !file_dirs.is_empty() {
                search.insert(file.path.clone(), file_dirs);
            }
        }

        Self {
            files,
            dirs,
            search,
        }
    }

    /// Whether top-level module `root` is a local file or directory on the
    /// `sys.path` pytest gives `file`.
    #[must_use]
    pub fn provides_root(&self, file: &str, root: &str) -> bool {
        self.search_dirs(file).any(|dir| {
            let base = join(dir, root);
            self.dirs.contains(&base)
                || self.files.contains(&format!("{base}.py"))
                || self.files.contains(&format!("{base}.pyi"))
        })
    }

    /// The `.py` file `module` names when imported from `file`, if any.
    #[must_use]
    pub fn resolve(&self, file: &str, module: &str) -> Option<&str> {
        let relative = module.replace('.', "/");
        self.search_dirs(file).find_map(|dir| {
            let base = join(dir, &relative);
            [format!("{base}.py"), format!("{base}/__init__.py")]
                .into_iter()
                .find_map(|candidate| self.files.get(&candidate).map(String::as_str))
        })
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
                start: std::env::temp_dir(),
            },
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
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
        };
        let paths = PytestImportPaths::with_settings(&sources(&E2E), &settings);
        assert_eq!(
            paths.resolve("tests/unit/test_core.py", "lifecycle"),
            Some("tests/e2e/lifecycle.py")
        );
        let importlib_only = PytestImportSettings {
            pythonpath: Vec::new(),
            importlib: true,
        };
        let paths = PytestImportPaths::with_settings(&sources(&E2E), &importlib_only);
        assert_eq!(
            paths.resolve("tests/e2e/access/test_access.py", "lifecycle"),
            None
        );
    }
}
