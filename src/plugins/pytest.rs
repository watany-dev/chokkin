//! pytest plugin extractor.

use std::path::Path;

use crate::config::PluginId;
use crate::fnmatch::fnmatch;
use crate::path_util::rel_to_root;
use crate::sources::{DiscoveredFile, FileContext, expand_testpath};

use super::config_text::ini_section_line;
use super::context::PluginContext;
use super::types::{
    BinaryUsage, ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin,
};
use super::util::{
    IniSection, join_lexical, origin_for_file, pytest_options_from_pyproject, read_ini_section,
    read_pyproject_table, toml_origin,
};
use super::warnings::PluginsWarning;

/// Extract pytest-related plugin hints.
#[expect(
    clippy::too_many_lines,
    reason = "reads every config surface the framework uses, in order; each block is short"
)]
pub(super) fn extract(ctx: &PluginContext<'_>) -> (PluginContribution, Vec<PluginsWarning>) {
    let mut contrib = PluginContribution::empty(PluginId::Pytest);
    let mut warnings = Vec::new();
    let root = ctx.root.path.as_path();

    let pyproject_path = root.join("pyproject.toml");
    let mut testpaths = Vec::new();
    let mut python_files = Vec::new();
    let mut has_explicit_config = false;
    let mut config_origin: Option<ReferenceOrigin> = None;
    let mut pyproject_table: Option<toml::Table> = None;

    if pyproject_path.is_file() {
        match read_pyproject_table(&pyproject_path) {
            Ok(table) => {
                if let Some((options, label)) = pytest_options_from_pyproject(&table) {
                    has_explicit_config = true;
                    config_origin = Some(toml_origin(root, &pyproject_path, label));
                    testpaths = str_list(options, "testpaths");
                    python_files = str_list(options, "python_files");
                }
                pyproject_table = Some(table);
            },
            Err(error) => {
                warnings.push(PluginsWarning::PluginExtractFailed {
                    plugin: PluginId::Pytest,
                    detail: error.to_string(),
                });
                return (contrib, warnings);
            },
        }
    }

    // Order is precedence. pytest selects `pytest.ini` as the config file even
    // without a `[pytest]` section, so only it warns when empty, and its
    // defaults then apply rather than a later file's options.
    for (file, section_name, label, selected_when_empty) in [
        ("pytest.ini", "pytest", "pytest.ini [pytest]", true),
        ("setup.cfg", "tool:pytest", "setup.cfg [tool:pytest]", false),
    ] {
        if config_origin.is_some() {
            break;
        }
        let path = root.join(file);
        if !path.is_file() {
            continue;
        }
        match read_ini_section(&path, section_name) {
            Ok(section) if section.is_empty() => {
                if selected_when_empty {
                    warnings.push(PluginsWarning::PytestConfigUnreadable {
                        path: rel_to_root(root, &path),
                    });
                    break;
                }
            },
            Ok(section) => {
                has_explicit_config = true;
                let mut origin = origin_for_file(root, &path, label);
                origin.line = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|contents| ini_section_line(&contents, section_name))
                    .and_then(|index| u32::try_from(index + 1).ok());
                config_origin = Some(origin);
                if let Some(value) = section.get("testpaths") {
                    testpaths = split_words(value);
                }
                if let Some(value) = section.get("python_files") {
                    python_files = split_words(value);
                }
            },
            Err(error) => {
                warnings.push(PluginsWarning::PluginExtractFailed {
                    plugin: PluginId::Pytest,
                    detail: error.to_string(),
                });
            },
        }
    }

    let origin = config_origin.take().unwrap_or_else(|| ReferenceOrigin {
        file: "pyproject.toml".to_owned(),
        line: None,
        label: "pytest defaults".to_owned(),
    });

    let collection = Collection::new(root, &testpaths, &python_files, &ctx.sources.files);
    let test_files = ctx.sources.files.iter().filter(|file| {
        collection.collects(&file.path)
            // Members' suites are collected by passing their directory to
            // pytest (airflow's `providers/*/tests`), so the root
            // `testpaths` does not limit them while `python_files` still
            // applies.
            || (file.context == FileContext::Test
                && ctx.sources.layout.member_for(&file.path).is_some()
                && collection.python_file(&file.path))
    });
    for file in test_files {
        contrib.entries.push(PluginEntry {
            spec: crate::config::EntrySpec {
                path: file.path.clone(),
                symbol: None,
            },
            context: FileContext::Test,
            origin: origin.clone(),
        });
    }

    let mut has_conftest = false;
    for file in &ctx.sources.files {
        if file.path.ends_with("/conftest.py") || file.path == "conftest.py" {
            has_conftest = true;
            contrib.entries.push(PluginEntry {
                spec: crate::config::EntrySpec {
                    path: file.path.clone(),
                    symbol: None,
                },
                context: FileContext::Test,
                origin: ReferenceOrigin {
                    file: file.path.clone(),
                    line: None,
                    label: "conftest.py".to_owned(),
                },
            });
        }
    }

    if let Some(table) = pyproject_table.as_ref()
        && let Some((options, label)) = pytest_options_from_pyproject(table)
        && let Some(plugins) = options.get("pytest_plugins")
    {
        // Each module takes the line of its own string (#882); the label
        // stays the table's, which messages name.
        let contents = std::fs::read_to_string(&pyproject_path).unwrap_or_default();
        for (module, item) in collect_pytest_plugins(plugins) {
            let path = format!("{label}.pytest_plugins{item}");
            contrib.module_refs.push(ModuleReference {
                module,
                origin: ReferenceOrigin {
                    line: crate::manifest::toml_label_line(&contents, &path),
                    ..origin.clone()
                },
            });
        }
    }

    // Test files matched by pytest's default globs alone are not evidence
    // that pytest is the runner: django and other unittest suites use the
    // same `test_*.py` names (#1241).
    contrib.binary_usages.push(BinaryUsage {
        binary: "pytest".to_owned(),
        origin,
        config_section: false,
        inferred: !has_explicit_config && !has_conftest,
    });

    if !has_explicit_config
        && contrib.entries.is_empty()
        && !super::util::manifest_has_dependency(ctx.manifest, "pytest")
    {
        contrib.binary_usages.clear();
    }

    (contrib, warnings)
}

/// The `sys.path` settings pytest applies while importing tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PytestImportSettings {
    /// `pythonpath` entries, relative to the project root.
    pub pythonpath: Vec<String>,
    /// `--import-mode=importlib` in `addopts`: test and conftest directories
    /// are not put on `sys.path`.
    pub importlib: bool,
    /// `testpaths` entries, relative to the config file's directory.
    pub testpaths: Vec<String>,
    /// `python_files` patterns, which replace pytest's `test_*.py` /
    /// `*_test.py` defaults (weblate's `tests.py`, #1242).
    pub python_files: Vec<String>,
    /// pytest-django's `--ds` in `addopts`, else its `DJANGO_SETTINGS_MODULE`
    /// option, with the name of the config file that sets it (#979, #1239).
    pub django_settings_module: Option<(String, String)>,
}

/// Read [`PytestImportSettings`] from the config file pytest itself picks in
/// `root`, or `None` when `root` holds no pytest config.
///
/// `pytest.ini` / `.pytest.ini` win even without a `[pytest]` section, then
/// the first of `pyproject.toml`, `tox.ini` and `setup.cfg` with a pytest
/// section.
#[must_use]
pub(crate) fn import_settings(root: &Path) -> Option<PytestImportSettings> {
    for file in ["pytest.ini", ".pytest.ini"] {
        let path = root.join(file);
        if path.is_file() {
            return Some(
                read_ini_section(&path, "pytest")
                    .map(|section| ini_import_settings(&section, file))
                    .unwrap_or_default(),
            );
        }
    }
    if let Some(settings) = pyproject_import_settings(&root.join("pyproject.toml")) {
        return Some(settings);
    }
    for (file, section_name) in [("tox.ini", "pytest"), ("setup.cfg", "tool:pytest")] {
        let path = root.join(file);
        if !path.is_file() {
            continue;
        }
        if let Ok(section) = read_ini_section(&path, section_name)
            && !section.is_empty()
        {
            return Some(ini_import_settings(&section, file));
        }
    }
    None
}

fn pyproject_import_settings(path: &Path) -> Option<PytestImportSettings> {
    if !path.is_file() {
        return None;
    }
    let table = read_pyproject_table(path).ok()?;
    let (options, _) = pytest_options_from_pyproject(&table)?;
    let addopts = options
        .get("addopts")
        .map_or_else(Vec::new, |value| match value {
            toml::Value::String(text) => split_words(text),
            toml::Value::Array(items) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        });
    Some(PytestImportSettings {
        pythonpath: str_list(options, "pythonpath"),
        importlib: has_importlib_mode(&addopts),
        testpaths: str_list(options, "testpaths"),
        python_files: str_list(options, "python_files"),
        django_settings_module: ds_option(&addopts)
            .or_else(|| {
                options
                    .get(DJANGO_SETTINGS_KEY)
                    .and_then(toml::Value::as_str)
                    .map(|value| value.trim().to_owned())
            })
            .map(|value| ("pyproject.toml".to_owned(), value)),
    })
}

/// The ini option pytest-django reads the settings module from.
const DJANGO_SETTINGS_KEY: &str = "DJANGO_SETTINGS_MODULE";

fn ini_import_settings(section: &IniSection, file: &str) -> PytestImportSettings {
    let words = |key: &str| section.get(key).map_or_else(Vec::new, |v| split_words(v));
    let addopts = words("addopts");
    PytestImportSettings {
        pythonpath: words("pythonpath"),
        importlib: has_importlib_mode(&addopts),
        testpaths: words("testpaths"),
        python_files: words("python_files"),
        django_settings_module: ds_option(&addopts)
            .or_else(|| {
                section
                    .get(DJANGO_SETTINGS_KEY)
                    .map(|value| value.trim().to_owned())
            })
            .map(|value| (file.to_owned(), value)),
    }
}

/// pytest-django's `--ds` in `addopts`, which outranks the ini option as a
/// command-line option does (#1239). The last one wins, as argparse's does.
fn ds_option(addopts: &[String]) -> Option<String> {
    addopts
        .iter()
        .enumerate()
        .filter_map(|(index, word)| match word.strip_prefix("--ds") {
            Some(value) => match value.strip_prefix('=') {
                Some(value) => Some(value.to_owned()),
                None if value.is_empty() => addopts.get(index + 1).cloned(),
                None => None,
            },
            None => None,
        })
        .next_back()
}

fn split_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn has_importlib_mode(addopts: &[String]) -> bool {
    addopts.iter().enumerate().any(|(index, word)| {
        word == "--import-mode=importlib"
            || (word == "--import-mode"
                && addopts
                    .get(index + 1)
                    .is_some_and(|next| next == "importlib"))
    })
}

/// Read a pytest option given either as a whitespace-separated string, which
/// pytest splits as it does an INI value, or as an array of strings.
fn str_list(options: &toml::Table, key: &str) -> Vec<String> {
    match options.get(key) {
        Some(toml::Value::String(value)) => split_words(value),
        Some(toml::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// Each module `pytest_plugins` names, with the suffix of its label: empty
/// for a lone string, `[index]` for an array element.
fn collect_pytest_plugins(value: &toml::Value) -> Vec<(String, String)> {
    match value {
        toml::Value::String(module) => vec![(module.clone(), String::new())],
        toml::Value::Array(items) => items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| Some((item.as_str()?.to_owned(), format!("[{index}]"))))
            .collect(),
        _ => Vec::new(),
    }
}

/// What pytest collects from the rootdir with these options, over the
/// discovered files. `testpaths` entries expand as `glob.iglob(entry,
/// recursive=True)` does (#1428); an entry naming a file collects that file
/// whatever `python_files` says (#1430); a file under an entry's directory
/// is collected when `python_files` names it as `fnmatch_ex` does (#1419).
/// When no entry names a discovered file, pytest searches the rootdir.
struct Collection {
    root: std::path::PathBuf,
    /// Root-relative directories and files; `[""]` is the rootdir.
    paths: Vec<String>,
    python_files: Vec<String>,
}

impl Collection {
    fn new(
        root: &Path,
        testpaths: &[String],
        python_files: &[String],
        files: &[DiscoveredFile],
    ) -> Self {
        let mut paths: Vec<String> = testpaths
            .iter()
            .filter_map(|entry| join_lexical("", entry))
            .flat_map(|entry| {
                if entry.contains(['*', '?', '[']) {
                    expand_testpath(root, &entry)
                } else {
                    vec![entry]
                }
            })
            .collect();
        let names_a_file = |path: &String| {
            files
                .iter()
                .any(|file| path.is_empty() || is_under(&file.path, path))
        };
        if !paths.iter().any(names_a_file) {
            paths = vec![String::new()];
        }
        let python_files = if python_files.is_empty() {
            vec!["test_*.py".to_owned(), "*_test.py".to_owned()]
        } else {
            python_files.to_vec()
        };
        Self {
            root: root.to_path_buf(),
            paths,
            python_files,
        }
    }

    fn collects(&self, path: &str) -> bool {
        self.paths.iter().any(|entry| {
            path == entry || ((entry.is_empty() || is_under(path, entry)) && self.python_file(path))
        })
    }

    /// `_pytest.pathlib.fnmatch_ex`: a pattern without `/` matches the
    /// basename; one with `/` matches the absolute path, `*/` prepended.
    fn python_file(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.python_files.iter().any(|pattern| {
            if pattern.contains('/') {
                let absolute = crate::path_util::normalize_rel_path(&self.root.join(path));
                if pattern.starts_with('/') {
                    fnmatch(pattern, &absolute)
                } else {
                    fnmatch(&format!("*/{pattern}"), &absolute)
                }
            } else {
                fnmatch(pattern, name)
            }
        })
    }
}

/// Whether root-relative `path` is `dir` or below it.
fn is_under(path: &str, dir: &str) -> bool {
    path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collection(
        root: &Path,
        testpaths: &[&str],
        python_files: &[&str],
        files: &[&str],
    ) -> Vec<String> {
        let files: Vec<DiscoveredFile> = files
            .iter()
            .map(|path| DiscoveredFile {
                path: (*path).to_owned(),
                kind: crate::sources::FileKind::Python,
                context: FileContext::Runtime,
            })
            .collect();
        let strings = |items: &[&str]| {
            items
                .iter()
                .map(|item| (*item).to_owned())
                .collect::<Vec<_>>()
        };
        let collection = Collection::new(root, &strings(testpaths), &strings(python_files), &files);
        files
            .iter()
            .map(|file| file.path.clone())
            .filter(|path| collection.collects(path))
            .collect()
    }

    fn tree(paths: &[&str]) -> tempfile::TempDir {
        let temp = tempfile::TempDir::new().expect("tempdir");
        for path in paths {
            let path = temp.path().join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, "").expect("write");
        }
        temp
    }

    /// pytest's defaults: no `testpaths` is the rootdir, and `python_files`
    /// is `test_*.py *_test.py`; `.` names the rootdir.
    #[test]
    fn collection_uses_defaults_and_dot_rootdir() {
        let files = [
            "a/test_x.py",
            "b_test.py",
            "a/check_y.py",
            "test/check_z.py",
        ];
        let temp = tree(&files);
        assert_eq!(
            collection(temp.path(), &[], &[], &files),
            ["a/test_x.py", "b_test.py"]
        );
        assert_eq!(
            collection(temp.path(), &[".", "./test/"], &["check_*.py"], &files),
            ["a/check_y.py", "test/check_z.py"]
        );
    }

    /// `python_files` is `fnmatch` on the basename: `*` never reaches a
    /// directory name, braces are literal, `[^x]` is a class of `^` and `x`
    /// (#1419).
    #[test]
    fn python_files_match_basenames_with_fnmatch() {
        let files = [
            "app/check_x/helper.py",
            "app/check_a.py",
            "app/check_{a,b}.py",
            "app/check_^.py",
        ];
        let temp = tree(&files);
        assert_eq!(
            collection(temp.path(), &[], &["check_*.py"], &files),
            ["app/check_a.py", "app/check_{a,b}.py", "app/check_^.py"]
        );
        assert_eq!(
            collection(temp.path(), &[], &["check_{a,b}.py"], &files),
            ["app/check_{a,b}.py"]
        );
        assert_eq!(
            collection(temp.path(), &[], &["check_[^x].py"], &files),
            ["app/check_^.py"]
        );
    }

    /// `testpaths` expand as Python's `glob` does (#1428), and a file they
    /// name is collected whatever `python_files` says while nothing else is
    /// (#1430).
    #[test]
    fn testpaths_expand_like_python_glob_and_name_files() {
        let files = ["t1/chk.py", "t{1,2}/chk.py", "a/chk.py", "b/chk.py"];
        let temp = tree(&files);
        assert_eq!(
            collection(temp.path(), &["t{1,2}"], &["chk.py"], &files),
            ["t{1,2}/chk.py"]
        );
        assert_eq!(
            collection(temp.path(), &["[^a]"], &["chk.py"], &files),
            ["a/chk.py"]
        );

        let files = ["t1/chk.py", "t1/other.py", "t2/chk.py"];
        let temp = tree(&files);
        for testpaths in [&["t1/*.py"][..], &["t1/chk.py", "t1/other.py"]] {
            assert_eq!(
                collection(temp.path(), testpaths, &["chk.py"], &files),
                ["t1/chk.py", "t1/other.py"],
                "testpaths {testpaths:?}"
            );
        }
        assert_eq!(
            collection(temp.path(), &["t1/chk.py"], &["chk.py"], &files),
            ["t1/chk.py"]
        );
        // An entry naming nothing leaves the rootdir, as pytest does.
        assert_eq!(
            collection(temp.path(), &["missing"], &["chk.py"], &files),
            ["t1/chk.py", "t2/chk.py"]
        );
    }

    #[test]
    fn import_settings_follow_pytest_config_precedence() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path();
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.pytest.ini_options]\npythonpath = [\"src\"]\ntestpaths = \"t/unit t/e2e\"\n",
        )
        .expect("write pyproject");
        assert_eq!(import_settings(root).expect("config").pythonpath, ["src"]);
        // pytest splits a string `testpaths` on whitespace, as in an ini file.
        assert_eq!(
            import_settings(root).expect("config").testpaths,
            ["t/unit", "t/e2e"]
        );

        std::fs::write(
            root.join("pytest.ini"),
            "[pytest]\naddopts = -q --import-mode importlib\n",
        )
        .expect("write pytest.ini");
        assert_eq!(
            import_settings(root),
            Some(PytestImportSettings {
                pythonpath: Vec::new(),
                importlib: true,
                testpaths: Vec::new(),
                python_files: Vec::new(),
                django_settings_module: None,
            })
        );
    }

    #[test]
    fn import_settings_skip_a_tox_ini_without_a_pytest_section() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path();
        std::fs::write(root.join("tox.ini"), "[tox]\nenv_list = py\n").expect("write tox.ini");
        assert_eq!(import_settings(root), None);
        std::fs::write(
            root.join("setup.cfg"),
            "[tool:pytest]\npythonpath = src\naddopts =\n    --import-mode=importlib\n",
        )
        .expect("write setup.cfg");
        assert_eq!(
            import_settings(root),
            Some(PytestImportSettings {
                pythonpath: vec!["src".to_owned()],
                importlib: true,
                ..PytestImportSettings::default()
            })
        );
    }

    #[test]
    fn importlib_mode_needs_the_option_and_its_value() {
        let importlib = |addopts: &str| has_importlib_mode(&split_words(addopts));
        assert!(importlib("-q --import-mode importlib"));
        assert!(importlib("--import-mode=importlib"));
        assert!(!importlib("--import-mode prepend"));
        assert!(!importlib("-p importlib"));
        assert!(!importlib("--import-mode"));
    }

    #[test]
    fn ds_option_outranks_the_ini_option() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path();
        let ds = |addopts: &str| {
            std::fs::write(
                root.join("pytest.ini"),
                format!("[pytest]\nDJANGO_SETTINGS_MODULE = acme.ini\naddopts = {addopts}\n"),
            )
            .expect("write pytest.ini");
            import_settings(root)
                .and_then(|settings| settings.django_settings_module)
                .map(|(_, module)| module)
        };
        assert_eq!(ds("-q --ds=acme.ds").as_deref(), Some("acme.ds"));
        assert_eq!(
            ds("--ds acme.one --ds=acme.two").as_deref(),
            Some("acme.two")
        );
        assert_eq!(ds("--dsx=acme.other -q").as_deref(), Some("acme.ini"));

        std::fs::remove_file(root.join("pytest.ini")).expect("remove pytest.ini");
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.pytest.ini_options]\naddopts = [\"--ds\", \"acme.ds\"]\n",
        )
        .expect("write pyproject");
        assert_eq!(
            import_settings(root).and_then(|settings| settings.django_settings_module),
            Some(("pyproject.toml".to_owned(), "acme.ds".to_owned()))
        );
    }

    #[test]
    fn import_settings_read_native_tool_pytest_table() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path();
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.pytest]\npythonpath = [\"src\"]\ntestpaths = [\"tests\"]\naddopts = [\"--import-mode=importlib\"]\n",
        )
        .expect("write pyproject");
        assert_eq!(
            import_settings(root),
            Some(PytestImportSettings {
                pythonpath: vec!["src".to_owned()],
                importlib: true,
                testpaths: vec!["tests".to_owned()],
                python_files: Vec::new(),
                django_settings_module: None,
            })
        );
    }

    #[test]
    fn import_settings_read_python_files() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path();
        std::fs::write(
            root.join("pytest.ini"),
            "[pytest]\npython_files = tests.py check_*.py\n",
        )
        .expect("write pytest.ini");
        assert_eq!(
            import_settings(root).expect("config").python_files,
            ["tests.py", "check_*.py"]
        );
        std::fs::remove_file(root.join("pytest.ini")).expect("remove pytest.ini");
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.pytest.ini_options]\npython_files = [\"tests.py\"]\n",
        )
        .expect("write pyproject");
        assert_eq!(
            import_settings(root).expect("config").python_files,
            ["tests.py"]
        );
    }
}
