//! pytest's default `--import-mode=prepend` (#360).
//!
//! Before importing a test module or `conftest.py`, pytest inserts its
//! basedir into `sys.path`: the file's own directory, or the parent of its
//! top-level package when the directory has an `__init__.py`. Files next to a
//! test or conftest therefore import under top-level names (`from lifecycle
//! import X`), which neither the layout nor the module index knows about.
//!
//! Two more lookups live here (#589): root directories the source globs never
//! walk (`docs_src/`, a src-layout project's `dummyserver/`) for tests whose
//! basedir is the root, and, as a fallback, a script's own directory, which
//! `python path/to/script.py` puts first.
//!
//! Last, the directories a file's own `sys.path` edits, or those of a
//! `conftest.py` above it, may add (#719): the edit's argument is often a
//! variable, so any path-like literal of the file names a candidate.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::parser::ParseSummary;
use crate::plugins::{PytestImportSettings, pytest_import_settings};
use crate::sources::{DiscoveredSources, FileContext, FileKind, path_to_module};

use super::types::import_root;

/// Extra import directories that apply to test-context files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PytestImportPaths {
    files: HashSet<String>,
    /// Non-test files run as scripts, with their own directory on `sys.path`.
    scripts: HashSet<String>,
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
    /// File → existing directories its `sys.path` edits may add.
    hinted: HashMap<String, Vec<String>>,
    /// Conftest directory → the same, for the files below it.
    conftest_hinted: Vec<(String, Vec<String>)>,
    /// Module names in hinted directories the walk skipped.
    hinted_on_disk: HashMap<String, HashSet<String>>,
    /// Hinted directory → top-level names of the discovered files under it.
    hinted_roots: HashMap<String, HashSet<String>>,
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
            let mut seen = HashSet::new();
            file_dirs.retain(|dir| seen.insert(dir.clone()));
            search.insert(file.path.clone(), file_dirs);
        }

        let scripts = sources
            .files
            .iter()
            .filter(|file| match file.context {
                FileContext::Test => false,
                FileContext::Runtime => path_to_module(&file.path, &sources.layout).is_none(),
                FileContext::Docs | FileContext::Dev => true,
            })
            .map(|file| file.path.clone())
            .collect();

        Self {
            files,
            scripts,
            dirs,
            search,
            root_modules: HashSet::new(),
            root_namespaces: HashSet::new(),
            hinted: HashMap::new(),
            conftest_hinted: Vec::new(),
            hinted_on_disk: HashMap::new(),
            hinted_roots: HashMap::new(),
        }
    }

    /// Record the directories each file's `sys.path` hints name: a discovered
    /// directory ending with the hint, or the hint under the file's directory
    /// or one of its ancestors, the root included, when it exists on disk.
    #[must_use]
    pub(crate) fn with_sys_path_hints(mut self, root: &Path, parse: &ParseSummary) -> Self {
        let mut by_name: HashMap<&str, Vec<&str>> = HashMap::new();
        for dir in &self.dirs {
            by_name.entry(file_name(dir)).or_default().push(dir);
        }
        for dirs in by_name.values_mut() {
            dirs.sort_unstable();
        }
        // Many files share hints and ancestors, so each ancestor is listed
        // once and each candidate checked once; most hints name nothing
        // there. Names are lowercased to stay a superset on case-insensitive
        // file systems.
        let mut listed: HashMap<&str, Option<HashSet<String>>> = HashMap::new();
        // Candidate → its module names, or `None` when it is no directory.
        let mut on_disk: HashMap<String, Option<HashSet<String>>> = HashMap::new();
        for module in &parse.modules {
            let holder_dir = parent_dir(&module.path);
            let mut found = Vec::new();
            for hint in &module.sys_path_hints {
                if hint.is_empty() {
                    found.push(holder_dir.to_owned());
                    continue;
                }
                for dir in by_name.get(file_name(hint)).into_iter().flatten() {
                    if ends_with_dirs(dir, hint) {
                        found.push((*dir).to_owned());
                    }
                }
                let head = hint
                    .split_once('/')
                    .map_or(hint.as_str(), |(head, _)| head)
                    .to_lowercase();
                let mut ancestor = holder_dir;
                loop {
                    let candidate = join(ancestor, hint);
                    if !self.dirs.contains(&candidate) {
                        let names = on_disk.entry(candidate.clone()).or_insert_with(|| {
                            let entries = listed
                                .entry(ancestor)
                                .or_insert_with(|| entry_names(&on_disk_path(root, ancestor)));
                            // A non-ASCII name may be stored in another
                            // Unicode normalization form, so only `stat` decides.
                            if head.is_ascii()
                                && entries.as_ref().is_some_and(|names| !names.contains(&head))
                            {
                                return None;
                            }
                            let path = on_disk_path(root, &candidate);
                            path.is_dir().then(|| module_names(&path))
                        });
                        if names.is_some() {
                            found.push(candidate);
                        }
                    }
                    if ancestor.is_empty() {
                        break;
                    }
                    ancestor = parent_dir(ancestor);
                }
            }
            if found.is_empty() {
                continue;
            }
            let mut seen = HashSet::new();
            found.retain(|dir| seen.insert(dir.clone()));
            if file_name(&module.path) == "conftest.py" {
                self.conftest_hinted.push((holder_dir.to_owned(), found));
            } else {
                self.hinted.insert(module.path.clone(), found);
            }
        }
        self.hinted_on_disk = on_disk
            .into_iter()
            .filter_map(|(dir, names)| Some((dir, names?)))
            .collect();
        self.hinted_roots = self.hinted_roots();
        self
    }

    fn hinted_roots(&self) -> HashMap<String, HashSet<String>> {
        let mut hinted_roots: HashMap<String, HashSet<String>> = self
            .hinted
            .values()
            .chain(self.conftest_hinted.iter().map(|(_, dirs)| dirs))
            .flatten()
            .map(|dir| (dir.clone(), HashSet::new()))
            .collect();
        if hinted_roots.is_empty() {
            return hinted_roots;
        }
        for file in &self.files {
            let mut dir = parent_dir(file);
            loop {
                if let Some(roots) = hinted_roots.get_mut(dir) {
                    let rest = if dir.is_empty() {
                        file.as_str()
                    } else {
                        &file[dir.len() + 1..]
                    };
                    let head = rest.split_once('/').map_or(rest, |(head, _)| head);
                    let root = head.strip_suffix(".py").unwrap_or(head);
                    if !roots.contains(root) {
                        roots.insert(root.to_owned());
                    }
                }
                if dir.is_empty() {
                    break;
                }
                dir = parent_dir(dir);
            }
        }
        hinted_roots
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
                if path.join("__init__.py").is_file() {
                    self.root_modules.insert(name);
                } else {
                    self.root_namespaces.insert(name);
                }
            } else if let Some(stem) = name
                .strip_suffix(".py")
                .or_else(|| name.strip_suffix(".pyi"))
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
    /// A directory a `sys.path` edit adds counts the same way (#719).
    #[must_use]
    pub(crate) fn provides_fallback(&self, file: &str, root: &str) -> bool {
        let local = if self.search.contains_key(file) {
            self.root_namespaces.contains(root)
        } else {
            self.scripts.contains(file) && self.has_module(parent_dir(file), root)
        };
        local
            || self.hinted_dirs(file).any(|dir| {
                self.has_module(dir, root)
                    || self
                        .hinted_on_disk
                        .get(dir)
                        .is_some_and(|names| names.contains(root))
            })
    }

    /// The file of `module` beside a script, or in a directory a `sys.path`
    /// edit adds.
    #[must_use]
    pub(crate) fn resolve_fallback(&self, file: &str, module: &str) -> Option<&str> {
        self.scripts
            .contains(file)
            .then(|| self.find_module(parent_dir(file), module))
            .flatten()
            .or_else(|| {
                let root = import_root(module);
                self.hinted_dirs(file)
                    // Most hinted directories hold no such name: skip them
                    // without building paths.
                    .filter(|dir| {
                        self.hinted_roots
                            .get(*dir)
                            .is_some_and(|roots| roots.contains(root))
                    })
                    .find_map(|dir| self.find_module(dir, module))
            })
    }

    fn hinted_dirs<'s>(&'s self, file: &'s str) -> impl Iterator<Item = &'s str> {
        let dir = parent_dir(file);
        let conftests = self
            .conftest_hinted
            .iter()
            .filter(move |(conftest_dir, _)| is_same_or_under(dir, conftest_dir))
            .map(|(_, dirs)| dirs);
        self.hinted
            .get(file)
            .into_iter()
            .chain(conftests)
            .flatten()
            .map(String::as_str)
    }

    fn has_module(&self, dir: &str, root: &str) -> bool {
        let base = join(dir, root);
        self.dirs.contains(&base)
            || self.files.contains(&format!("{base}.py"))
            || self.files.contains(&format!("{base}.pyi"))
    }

    fn find_module(&self, dir: &str, module: &str) -> Option<&str> {
        // One buffer for both candidates: this runs for every unresolved
        // import of a file with hinted directories.
        let mut path = String::with_capacity(dir.len() + module.len() + "//__init__.py".len());
        if !dir.is_empty() {
            path.push_str(dir);
            path.push('/');
        }
        path.extend(module.chars().map(|c| if c == '.' { '/' } else { c }));
        let base = path.len();
        [".py", "/__init__.py"].into_iter().find_map(|suffix| {
            path.truncate(base);
            path.push_str(suffix);
            self.files.get(&path).map(String::as_str)
        })
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

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Whether `dir` is `suffix` or ends with it at a segment boundary.
fn ends_with_dirs(dir: &str, suffix: &str) -> bool {
    dir.strip_suffix(suffix)
        .is_some_and(|rest| rest.is_empty() || rest.ends_with('/'))
}

fn on_disk_path(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .filter(|segment| !segment.is_empty())
        .fold(root.to_path_buf(), |path, segment| path.join(segment))
}

/// Lowercased entry names in `dir`, or `None` when it cannot be listed.
fn entry_names(dir: &Path) -> Option<HashSet<String>> {
    let entries = std::fs::read_dir(dir).ok()?;
    Some(
        entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_lowercase())
            .collect(),
    )
}

/// Top-level module names in `dir`: subdirectories and `.py` / `.pyi` stems.
fn module_names(dir: &Path) -> HashSet<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                return Some(name);
            }
            name.strip_suffix(".py")
                .or_else(|| name.strip_suffix(".pyi"))
                .map(str::to_owned)
        })
        .collect()
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
                ..Default::default()
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
        let files = [
            "src/acme/__init__.py",
            "tests/__init__.py",
            "tests/unit/__init__.py",
            "tests/unit/test_core.py",
            "tests/other/test_other.py",
        ];
        let paths =
            PytestImportPaths::with_settings(&sources(&files), &PytestImportSettings::default())
                .with_root_entries(temp.path());
        let test = "tests/unit/test_core.py";
        assert!(paths.provides_root(test, "dummyserver"));
        assert!(paths.provides_root(test, "rootmod"));
        // A namespace portion loses to a regular package anywhere on the path.
        assert!(!paths.provides_root(test, "docs_src"));
        assert!(paths.provides_fallback(test, "docs_src"));
        // A test whose basedir is not the root does not see root entries.
        assert!(!paths.provides_root("tests/other/test_other.py", "dummyserver"));
        // Runtime code does not run with the root on its path.
        assert!(!paths.provides_root("src/acme/__init__.py", "dummyserver"));
        assert!(!paths.provides_fallback("src/acme/__init__.py", "docs_src"));
    }

    #[test]
    fn scripts_fall_back_to_modules_beside_them() {
        let files = [
            "src/acme/__init__.py",
            "src/acme/run.py",
            "src/acme/helpers.py",
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
            paths.resolve_fallback(script, "common_utils"),
            Some("scripts/ci/prek/common_utils.py")
        );
        assert!(!paths.provides_root(script, "common_utils"));
        // A runtime module imports by its package name, not its directory.
        assert!(!paths.provides_fallback("src/acme/run.py", "helpers"));
        assert_eq!(paths.resolve_fallback("src/acme/run.py", "helpers"), None);
        // Tests follow pytest's rules, not the script fallback.
        assert!(paths.provides_root("tests/e2e/test_x.py", "helpers"));
        assert!(!paths.provides_fallback("tests/pkg/test_y.py", "helpers"));
        assert_eq!(
            paths.resolve_fallback("tests/pkg/test_y.py", "helpers"),
            None
        );
    }

    fn hinting(path: &str, hints: &[&str]) -> crate::parser::ParsedModule {
        crate::parser::ParsedModule {
            path: path.to_owned(),
            sys_path_hints: hints.iter().map(|hint| (*hint).to_owned()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn sys_path_hints_name_directories_with_the_imported_module() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("utils")).expect("mkdir");
        std::fs::write(temp.path().join("utils/check_docs.py"), "").expect("write");
        let files = [
            "src/acme/__init__.py",
            "scripts/ci/prek/common_utils.py",
            "scripts/run.py",
            "tests/test_util.py",
            "tests/plain/test_plain.py",
            "tests/cli/conftest.py",
            "tests/cli/test_cli.py",
            "tests/cli/test_apps/cliapp/__init__.py",
            "src/acme/tool.py",
            "src/acme/sibling.py",
            "scripts/lint.py",
            "tools/prek/tool_utils.py",
        ];
        let parse = ParseSummary {
            modules: vec![
                hinting("tests/test_util.py", &["utils", "missing"]),
                hinting("scripts/run.py", &["prek"]),
                hinting("scripts/lint.py", &["ci/prek"]),
                hinting("tests/cli/conftest.py", &["test_apps"]),
                hinting("src/acme/tool.py", &[""]),
            ],
        };
        let paths =
            PytestImportPaths::with_settings(&sources(&files), &PytestImportSettings::default())
                .with_sys_path_hints(temp.path(), &parse);
        // An undiscovered directory under an ancestor, checked on disk.
        assert!(paths.provides_fallback("tests/test_util.py", "check_docs"));
        assert!(!paths.provides_fallback("tests/test_util.py", "check_doc"));
        // Hints naming no existing directory add nothing.
        assert_eq!(paths.hinted["tests/test_util.py"], ["utils"]);
        // A discovered directory ending with the hint.
        assert!(paths.provides_fallback("scripts/run.py", "common_utils"));
        assert_eq!(
            paths.resolve_fallback("scripts/run.py", "common_utils"),
            Some("scripts/ci/prek/common_utils.py")
        );
        // A multi-segment hint matches whole trailing segments only.
        assert!(paths.provides_fallback("scripts/lint.py", "common_utils"));
        assert!(!paths.provides_fallback("scripts/lint.py", "tool_utils"));
        // A conftest's hints apply to the files below it only.
        assert!(paths.provides_fallback("tests/cli/test_cli.py", "cliapp"));
        assert_eq!(
            paths.resolve_fallback("tests/cli/test_cli.py", "cliapp"),
            Some("tests/cli/test_apps/cliapp/__init__.py")
        );
        assert!(!paths.provides_fallback("tests/plain/test_plain.py", "cliapp"));
        // `"."` names the file's own directory.
        assert!(paths.provides_fallback("src/acme/tool.py", "sibling"));
        assert_eq!(
            paths.resolve_fallback("src/acme/tool.py", "sibling"),
            Some("src/acme/sibling.py")
        );
        // A file without hints is unaffected.
        assert!(!paths.provides_fallback("tests/plain/test_plain.py", "check_docs"));
        // Only names a hinted directory holds are looked up there.
        let roots = |dir: &str| {
            let mut roots: Vec<&str> = paths.hinted_roots[dir].iter().map(String::as_str).collect();
            roots.sort_unstable();
            roots
        };
        assert_eq!(roots("scripts/ci/prek"), ["common_utils"]);
        assert_eq!(roots("tests/cli/test_apps"), ["cliapp"]);
        assert_eq!(roots("src/acme"), ["__init__", "sibling", "tool"]);
        assert_eq!(roots("utils"), [] as [&str; 0]);
    }

    #[test]
    fn root_conftest_hints_apply_to_every_file() {
        let files = [
            "conftest.py",
            "helpers/__init__.py",
            "helpers/fixtures.py",
            "tests/models/test_model.py",
        ];
        let parse = ParseSummary {
            modules: vec![hinting("conftest.py", &[""])],
        };
        let temp = tempfile::TempDir::new().expect("tempdir");
        let paths =
            PytestImportPaths::with_settings(&sources(&files), &PytestImportSettings::default())
                .with_sys_path_hints(temp.path(), &parse);
        let test = "tests/models/test_model.py";
        assert_eq!(
            paths.resolve_fallback(test, "helpers.fixtures"),
            Some("helpers/fixtures.py")
        );
        assert_eq!(
            paths.resolve_fallback(test, "conftest"),
            Some("conftest.py")
        );
        assert_eq!(paths.resolve_fallback(test, "helpers.missing"), None);
        assert_eq!(paths.resolve_fallback(test, "torch"), None);
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
