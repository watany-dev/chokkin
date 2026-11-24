//! `.venv` dist-info metadata reading.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::discovery::ProjectRoot;
use crate::manifest::{is_project_name, normalize_distribution_name};
use crate::path_util::rel_to_root;

use super::metadata::parse_metadata;
use super::types::ResolveWarning;

/// Import root → distribution names discovered from a project virtualenv.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VenvIndex {
    /// Import root to normalized distribution names.
    pub imports: BTreeMap<String, Vec<String>>,
    /// Dotted module (2 segments or more) to the distributions owning the
    /// file it loads (its `__init__` or module file), kept only under a root
    /// several distributions share: a PEP 420 namespace's portions (#1391).
    pub modules: BTreeMap<String, Vec<String>>,
    /// Console script name to normalized distribution names.
    pub binaries: BTreeMap<String, String>,
    /// Distributions registering a `pytest11` entry point (auto-loaded pytest plugins).
    pub pytest_plugins: BTreeSet<String>,
}

/// Load import mappings from a project virtualenv when present.
#[must_use]
pub(super) fn load_venv_index(root: &ProjectRoot, warnings: &mut Vec<ResolveWarning>) -> VenvIndex {
    let Some(venv_path) = discover_venv(&root.path) else {
        return VenvIndex::default();
    };

    match read_venv_index(&root.path, &venv_path, warnings) {
        Ok(index) => index,
        Err(reason) => {
            warnings.push(ResolveWarning::VenvUnreadable {
                path: rel_to_root(&root.path, &venv_path),
                reason,
            });
            VenvIndex::default()
        },
    }
}

fn discover_venv(root: &Path) -> Option<PathBuf> {
    for name in [".venv", "venv"] {
        let candidate = root.join(name);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

fn read_venv_index(
    root: &Path,
    venv: &Path,
    warnings: &mut Vec<ResolveWarning>,
) -> Result<VenvIndex, String> {
    let site_packages = find_site_packages(root, venv, warnings)
        .ok_or_else(|| "site-packages not found".to_owned())?;
    let mut index = VenvIndex::default();

    let entries = fs::read_dir(&site_packages).map_err(|err| err.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let layout = if file_name.ends_with(".dist-info") {
            Layout::DistInfo
        } else if file_name.ends_with(".egg-info") {
            Layout::EggInfo
        } else {
            continue;
        };
        match merge_distribution(&path, layout, &mut index) {
            Ok(Read::Imports) => {},
            Ok(Read::NoImportNames) => warnings.push(ResolveWarning::VenvDistributionUnread {
                path: rel_to_root(root, &path),
            }),
            Err(name) => warnings.push(ResolveWarning::VenvDistributionIgnored {
                path: rel_to_root(root, &path),
                name,
            }),
        }
    }

    for imports in index.imports.values_mut().chain(index.modules.values_mut()) {
        imports.sort();
        imports.dedup();
    }
    let imports = &index.imports;
    index.modules.retain(|module, _| {
        imports
            .get(import_root(module))
            .is_some_and(|owners| owners.len() > 1)
    });
    Ok(index)
}

/// How `importlib.metadata` finds a distribution in site-packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// `<name>-<version>.dist-info/` with `METADATA` and a CSV `RECORD` of
    /// site-packages-relative paths.
    DistInfo,
    /// `<name>-<version>[-pyX.Y].egg-info`, a directory with `PKG-INFO` and
    /// `installed-files.txt` (relative to itself), or a lone `PKG-INFO` file
    /// from distutils (#1406).
    EggInfo,
}

/// What reading one distribution learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// Its import names, possibly none (a RECORD naming no module).
    Imports,
    /// An egg-info with no file list or `top_level.txt`: its import names are
    /// unknown, so its imports resolve as without a virtualenv.
    NoImportNames,
}

fn import_root(module: &str) -> &str {
    module.split('.').next().unwrap_or(module)
}

/// A POSIX venv keeps one `lib/pythonX.Y/site-packages` per interpreter it
/// was built with, and `--upgrade` leaves the old one behind. `read_dir`
/// order is the filesystem's, so the one matching `pyvenv.cfg` wins; without
/// a single match the highest version wins and the rest are reported (#1169).
fn find_site_packages(
    root: &Path,
    venv: &Path,
    warnings: &mut Vec<ResolveWarning>,
) -> Option<PathBuf> {
    // Windows venvs use Lib/site-packages (no pythonX.Y segment).
    let windows_style = venv.join("Lib").join("site-packages");
    if windows_style.is_dir() {
        return Some(windows_style);
    }

    let lib = venv.join("lib");
    let mut candidates: Vec<(Option<(u32, u32)>, String)> = fs::read_dir(&lib)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().join("site-packages").is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .map(|name| (interpreter_version(&name), name))
        .collect();
    let configured = configured_version(venv);
    let matching: Vec<_> = candidates
        .iter()
        .filter(|(version, _)| version.is_some() && *version == configured)
        .cloned()
        .collect();
    if !matching.is_empty() {
        candidates = matching;
    }
    candidates.sort();
    let (_, chosen) = candidates.last()?;
    if candidates.len() > 1 {
        warnings.push(ResolveWarning::VenvSitePackagesAmbiguous {
            path: rel_to_root(root, &lib),
            chosen: chosen.clone(),
            candidates: candidates.iter().map(|(_, name)| name.clone()).collect(),
        });
    }
    Some(lib.join(chosen).join("site-packages"))
}

/// `X.Y` from `pyvenv.cfg`: uv writes `version_info`, `python -m venv`
/// writes `version`. A file without either decides nothing.
fn configured_version(venv: &Path) -> Option<(u32, u32)> {
    let contents = fs::read_to_string(venv.join("pyvenv.cfg")).ok()?;
    let mut version = None;
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "version_info" => return major_minor(value.trim()),
            "version" => version = major_minor(value.trim()),
            _ => {},
        }
    }
    version
}

/// `python3.12` / `python3.13t` / `pypy3.10` → `(3, 12)`; any other
/// directory name has no version and sorts before every versioned one.
fn interpreter_version(dir_name: &str) -> Option<(u32, u32)> {
    let rest = dir_name
        .strip_prefix("python")
        .or_else(|| dir_name.strip_prefix("pypy"))?;
    major_minor(rest.strip_suffix('t').unwrap_or(rest))
}

fn major_minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts
        .next()?
        .split(|ch: char| !ch.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor))
}

/// `Err` carries a name that is not a PEP 508 project name: pip installs
/// none such, and trusting one credited `yaml` to `evil\x1b[31m` (#973).
fn merge_distribution(dir: &Path, layout: Layout, index: &mut VenvIndex) -> Result<Read, String> {
    let lone_file = layout == Layout::EggInfo && dir.is_file();
    let metadata_path = match layout {
        Layout::DistInfo => dir.join("METADATA"),
        Layout::EggInfo if lone_file => dir.to_path_buf(),
        Layout::EggInfo => dir.join("PKG-INFO"),
    };
    let Ok(contents) = fs::read_to_string(&metadata_path) else {
        return Ok(Read::Imports);
    };
    let metadata = parse_metadata(&contents);
    let name = metadata.name.clone().or_else(|| {
        dir.file_name()
            .and_then(|name| name.to_str())
            .and_then(distribution_dir_name)
            .map(str::to_owned)
    });
    let Some(name) = name else {
        return Ok(Read::Imports);
    };
    if !is_project_name(&name) {
        return Err(name);
    }
    let distribution = normalize_distribution_name(&name);

    // A lone-file egg-info has no `top_level.txt` or file list beside it:
    // reading under a file fails, so neither needs its own guard.
    let top_level = fs::read_to_string(dir.join("top_level.txt")).ok();
    for line in top_level.iter().flat_map(|top_level| top_level.lines()) {
        let import = line.trim();
        if !import.is_empty() && !import.starts_with('_') {
            push_import(index, import, &distribution);
        }
    }

    for import in metadata
        .import_names
        .iter()
        .chain(&metadata.import_namespaces)
    {
        push_import(index, import_root(import), &distribution);
    }
    // An `Import-Namespace` loads no file, so it names no module's owner.
    for import in &metadata.import_names {
        if import.contains('.') {
            push_module(index, import, &distribution);
        }
    }

    let files = match layout {
        Layout::DistInfo => fs::read_to_string(dir.join("RECORD")).ok(),
        Layout::EggInfo => fs::read_to_string(dir.join("installed-files.txt")).ok(),
    };
    for entry in files
        .iter()
        .flat_map(|files| installed_modules(files, layout))
    {
        push_import(index, &entry.root, &distribution);
        if let Some(module) = entry.module.filter(|module| module.contains('.')) {
            push_module(index, &module, &distribution);
        }
    }

    merge_entry_points(&dir.join("entry_points.txt"), &distribution, index);
    let unread = layout == Layout::EggInfo
        && files.is_none()
        && top_level.is_none()
        && metadata.import_names.is_empty();
    Ok(if unread {
        Read::NoImportNames
    } else {
        Read::Imports
    })
}

fn merge_entry_points(path: &Path, distribution: &str, index: &mut VenvIndex) {
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    for script in entry_point_names(&contents, "console_scripts") {
        index.binaries.insert(script, distribution.to_owned());
    }
    if !entry_point_names(&contents, "pytest11").is_empty() {
        index.pytest_plugins.insert(distribution.to_owned());
    }
}

fn push_import(index: &mut VenvIndex, import: &str, distribution: &str) {
    index
        .imports
        .entry(import.to_owned())
        .or_default()
        .push(distribution.to_owned());
}

fn push_module(index: &mut VenvIndex, module: &str, distribution: &str) {
    index
        .modules
        .entry(module.to_owned())
        .or_default()
        .push(distribution.to_owned());
}

/// The distribution name a `.dist-info` (`<name>-<version>`) or `.egg-info`
/// (`<name>-<version>[-pyX.Y]`, or a bare `<name>` from a develop install)
/// directory carries, used when its metadata has no `Name:`.
fn distribution_dir_name(file_name: &str) -> Option<&str> {
    if let Some(stem) = file_name.strip_suffix(".dist-info") {
        return stem.rsplit_once('-').map(|(name, _version)| name);
    }
    let stem = file_name.strip_suffix(".egg-info")?;
    Some(stem.split_once('-').map_or(stem, |(name, _)| name))
}

/// One importable file a distribution installed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct InstalledModule {
    /// Top-level import name: the first directory, or the module a file
    /// directly in site-packages defines.
    root: String,
    /// Dotted module the file loads, `None` for a data file in a package.
    module: Option<String>,
}

/// The modules a `RECORD` (CSV, site-packages-relative) or egg-info
/// `installed-files.txt` (one path per line, relative to the egg-info
/// directory) lists. Paths outside site-packages (`../../bin/x`), metadata
/// and files no import loads (`py.typed`, `.pth`, `.pyi`, a directory that is
/// no identifier) name no module: no `import` statement can spell them.
fn installed_modules(contents: &str, layout: Layout) -> BTreeSet<InstalledModule> {
    let mut modules = BTreeSet::new();
    for line in contents.lines() {
        let path = match layout {
            Layout::DistInfo => csv_first_field(line),
            Layout::EggInfo => match line.trim().strip_prefix("../") {
                Some(path) => path.to_owned(),
                None => continue,
            },
        };
        if let Some(module) = installed_module(&path)
            && !module.root.starts_with('_')
        {
            modules.insert(module);
        }
    }
    modules
}

/// First field of a CSV row: `csv.writer` may quote any field, and must
/// when the path holds a `,`; `""` escapes a quote inside (#1404).
fn csv_first_field(line: &str) -> String {
    let line = line.trim();
    let Some(quoted) = line.strip_prefix('"') else {
        return line.split(',').next().unwrap_or_default().trim().to_owned();
    };
    let mut field = String::new();
    let mut chars = quoted.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' && chars.next_if_eq(&'"').is_none() {
            break;
        }
        field.push(ch);
    }
    field
}

fn installed_module(path: &str) -> Option<InstalledModule> {
    let path = path.trim().trim_start_matches("./");
    let segments: Vec<&str> = path.split('/').collect();
    let (file, dirs) = segments.split_last()?;
    let Some(&root) = dirs.first() else {
        let module = module_of_file(file)?;
        return Some(InstalledModule {
            root: module.to_owned(),
            module: Some(module.to_owned()),
        });
    };
    if !is_identifier(root) {
        return None;
    }
    let module = (dirs.iter().all(|dir| is_identifier(dir)))
        .then(|| module_of_file(file))
        .flatten()
        .map(|name| {
            let package = dirs.join(".");
            if name == "__init__" {
                package
            } else {
                format!("{package}.{name}")
            }
        });
    Some(InstalledModule {
        root: root.to_owned(),
        module,
    })
}

/// The module a file name defines: `m.py`, or an extension module
/// (`m.cpython-312-x86_64-linux-gnu.so`, `m.abi3.so`, `m.so`, `m.cp312-win_amd64.pyd`,
/// `m.pyd`) up to its first `.` (#1401).
fn module_of_file(file: &str) -> Option<&str> {
    let name = match file.rsplit_once('.')? {
        (stem, "py") => stem,
        (_, "so" | "pyd") => file.split('.').next()?,
        _ => return None,
    };
    is_identifier(name).then_some(name)
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_alphanumeric() || ch == '_')
}

fn entry_point_names(contents: &str, group: &str) -> Vec<String> {
    let mut scripts = Vec::new();
    let mut in_group = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_group = trimmed
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .is_some_and(|name| name.trim().eq_ignore_ascii_case(group));
            continue;
        }
        if !in_group || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = trimmed.split_once('=') {
            let name = name.trim();
            if !name.is_empty() {
                scripts.push(name.to_owned());
            }
        }
    }
    scripts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_roots(record: &str) -> Vec<String> {
        let roots: BTreeSet<String> = installed_modules(record, Layout::DistInfo)
            .into_iter()
            .map(|module| module.root)
            .collect();
        roots.into_iter().collect()
    }

    #[test]
    fn parses_dist_info_directory_name() {
        assert_eq!(
            distribution_dir_name("PyYAML-6.0.1.dist-info"),
            Some("PyYAML")
        );
        assert_eq!(
            distribution_dir_name("acme_img-1.0-py3.12.egg-info"),
            Some("acme_img")
        );
        assert_eq!(distribution_dir_name("acme_img.egg-info"), Some("acme_img"));
    }

    /// A `Name:` outside PEP 508, from METADATA or the directory, credits
    /// nothing and is handed back for the warning (#973).
    #[test]
    fn dist_info_with_an_invalid_name_is_ignored() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        for (dir_name, metadata) in [
            ("evil-1.0.dist-info", "Name: evil\x1b[31mRED\x1b[0m\n"),
            ("bad name-1.0.dist-info", "Version: 1.0\n"),
            ("_hidden-1.0.dist-info", "Name: _hidden\n"),
        ] {
            let dist_info = dir.path().join(dir_name);
            std::fs::create_dir(&dist_info).expect("mkdir");
            std::fs::write(dist_info.join("METADATA"), metadata).expect("write");
            std::fs::write(dist_info.join("top_level.txt"), "yaml\n").expect("write");
            std::fs::write(
                dist_info.join("entry_points.txt"),
                "[console_scripts]\nyaml = yaml:main\n",
            )
            .expect("write");
            let mut index = VenvIndex::default();
            assert!(
                merge_distribution(&dist_info, Layout::DistInfo, &mut index).is_err(),
                "{dir_name}"
            );
            assert_eq!(index, VenvIndex::default(), "{dir_name}");
        }
    }

    #[test]
    fn venv_index_warns_about_each_ignored_dist_info() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let site = dir.path().join(".venv/lib/python3.12/site-packages");
        for (name, metadata) in [("evil", "Name: evil\x07\n"), ("good", "Name: good\n")] {
            let dist_info = site.join(format!("{name}-1.0.dist-info"));
            std::fs::create_dir_all(&dist_info).expect("mkdir");
            std::fs::write(dist_info.join("METADATA"), metadata).expect("write");
        }
        let mut warnings = Vec::new();
        read_venv_index(dir.path(), &dir.path().join(".venv"), &mut warnings).expect("read");
        assert_eq!(
            warnings,
            [ResolveWarning::VenvDistributionIgnored {
                path: ".venv/lib/python3.12/site-packages/evil-1.0.dist-info".to_owned(),
                name: "evil\x07".to_owned(),
            }]
        );
    }

    fn venv_with_interpreters(dirs: &[&str], cfg: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let venv = dir.path().join(".venv");
        for name in dirs {
            std::fs::create_dir_all(venv.join("lib").join(name).join("site-packages"))
                .expect("mkdir");
        }
        if let Some(cfg) = cfg {
            std::fs::write(venv.join("pyvenv.cfg"), cfg).expect("write");
        }
        dir
    }

    fn chosen_interpreter(dir: &tempfile::TempDir) -> (String, Vec<ResolveWarning>) {
        let mut warnings = Vec::new();
        let site = find_site_packages(dir.path(), &dir.path().join(".venv"), &mut warnings)
            .expect("site-packages");
        let interpreter = site
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .expect("interpreter dir")
            .to_owned();
        (interpreter, warnings)
    }

    /// `pyvenv.cfg` picks the interpreter a `--upgrade` left behind, whatever
    /// order `read_dir` lists them in (#1169).
    #[test]
    fn pyvenv_cfg_version_picks_the_site_packages() {
        for cfg in [
            "home = /usr/bin\nversion = 3.11.9\n",
            "home = /usr/bin\nversion_info = 3.11.9\nversion = 3.12.0\n",
        ] {
            let dir = venv_with_interpreters(&["python3.11", "python3.12"], Some(cfg));
            assert_eq!(
                chosen_interpreter(&dir),
                ("python3.11".to_owned(), Vec::new()),
                "{cfg}"
            );
        }
    }

    /// Without a deciding `pyvenv.cfg` the highest version is read, not the
    /// lexically last (`python3.9` > `python3.10`), and the choice is reported.
    #[test]
    fn undecided_interpreters_read_the_highest_and_warn() {
        for cfg in [None, Some("version = 3.8.1\n"), Some("garbage\n")] {
            let dir = venv_with_interpreters(&["python3.9", "python3.10"], cfg);
            assert_eq!(
                chosen_interpreter(&dir),
                (
                    "python3.10".to_owned(),
                    vec![ResolveWarning::VenvSitePackagesAmbiguous {
                        path: ".venv/lib".to_owned(),
                        chosen: "python3.10".to_owned(),
                        candidates: vec!["python3.9".to_owned(), "python3.10".to_owned()],
                    }]
                ),
                "{cfg:?}"
            );
        }
    }

    /// The default and free-threaded builds of one version both match it.
    #[test]
    fn one_interpreter_needs_no_cfg_and_two_builds_of_one_version_warn() {
        let dir = venv_with_interpreters(&["python3.12"], None);
        assert_eq!(
            chosen_interpreter(&dir),
            ("python3.12".to_owned(), Vec::new())
        );

        let dir = venv_with_interpreters(
            &["python3.12", "python3.13", "python3.13t"],
            Some("version_info = 3.13.0\n"),
        );
        let (chosen, warnings) = chosen_interpreter(&dir);
        assert_eq!(chosen, "python3.13t");
        assert!(matches!(
            warnings.as_slice(),
            [ResolveWarning::VenvSitePackagesAmbiguous { candidates, .. }]
                if candidates == &["python3.13".to_owned(), "python3.13t".to_owned()]
        ));
    }

    #[test]
    fn interpreter_dir_names_parse_their_version() {
        assert_eq!(interpreter_version("python3.12"), Some((3, 12)));
        assert_eq!(interpreter_version("python3.13t"), Some((3, 13)));
        assert_eq!(interpreter_version("pypy3.10"), Some((3, 10)));
        assert_eq!(interpreter_version("site-python"), None);
        assert_eq!(major_minor("3.12.0rc1"), Some((3, 12)));
        assert_eq!(major_minor("3.14rc1"), Some((3, 14)));
    }

    #[test]
    fn parses_record_top_level_imports() {
        assert_eq!(
            record_roots("yaml/__init__.py,sha256=abc,100\nyaml/loader.py,sha256=def,200\n"),
            vec!["yaml".to_owned()]
        );
    }

    #[test]
    fn extension_modules_name_their_import_and_other_files_none() {
        for (file, module) in [
            ("m.py", Some("m")),
            ("m.cpython-312-x86_64-linux-gnu.so", Some("m")),
            ("m.abi3.so", Some("m")),
            ("m.so", Some("m")),
            ("m.cp312-win_amd64.pyd", Some("m")),
            ("m.pyd", Some("m")),
            ("m.pyi", None),
            ("py.typed", None),
            ("m.pth", None),
            ("1m.py", None),
            ("my-mod.py", None),
        ] {
            assert_eq!(module_of_file(file), module, "{file}");
        }
    }

    #[test]
    fn record_rows_are_csv() {
        assert_eq!(
            csv_first_field("acme/__init__.py,sha256=x,0"),
            "acme/__init__.py"
        );
        assert_eq!(
            csv_first_field("\"acme/__init__.py\",sha256=x,0"),
            "acme/__init__.py"
        );
        assert_eq!(csv_first_field("\"acme/a,b.txt\",,"), "acme/a,b.txt");
        assert_eq!(
            csv_first_field("\"acme/\"\"q\"\".txt\",,"),
            "acme/\"q\".txt"
        );
    }

    #[test]
    fn installed_files_name_the_module_they_load() {
        let module = |path: &str| installed_module(path).and_then(|module| module.module);
        assert_eq!(
            module("acme/billing/__init__.py").as_deref(),
            Some("acme.billing")
        );
        assert_eq!(
            module("acme/billing/api.py").as_deref(),
            Some("acme.billing.api")
        );
        assert_eq!(module("acme/billing/data.json"), None);
        assert_eq!(module("acme/my-dir/x.py"), None);
        assert_eq!(installed_module("acme-1.0.data/x.py"), None);
        assert_eq!(
            installed_modules(
                "../acme/__init__.py\nPKG-INFO\n../../../bin/acme\n",
                Layout::EggInfo
            ),
            BTreeSet::from([InstalledModule {
                root: "acme".to_owned(),
                module: Some("acme".to_owned()),
            }])
        );
    }

    /// Module owners are kept only under a root several distributions share.
    #[test]
    fn module_index_keeps_shared_roots_only() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let site = dir.path().join(".venv/lib/python3.12/site-packages");
        for (name, record) in [
            ("auth", "acme/auth/__init__.py,,\n"),
            (
                "billing",
                "acme/billing/__init__.py,,\nacme/billing/api.py,,\n",
            ),
            ("solo", "solo/core/__init__.py,,\n"),
        ] {
            let dist_info = site.join(format!("{name}-1.0.dist-info"));
            std::fs::create_dir_all(&dist_info).expect("mkdir");
            std::fs::write(dist_info.join("METADATA"), format!("Name: {name}\n")).expect("write");
            std::fs::write(dist_info.join("RECORD"), record).expect("write");
        }
        let index =
            read_venv_index(dir.path(), &dir.path().join(".venv"), &mut Vec::new()).expect("read");
        assert_eq!(
            index.modules,
            BTreeMap::from([
                ("acme.auth".to_owned(), vec!["auth".to_owned()]),
                ("acme.billing".to_owned(), vec!["billing".to_owned()]),
                ("acme.billing.api".to_owned(), vec!["billing".to_owned()]),
            ])
        );
    }

    #[test]
    fn parses_console_scripts_section() {
        let contents = "[console_scripts]\nflake8 = flake8.main:main\n\n[other]\n";
        assert_eq!(
            entry_point_names(contents, "console_scripts"),
            vec!["flake8".to_owned()]
        );
    }

    #[test]
    fn parses_pytest11_section() {
        let contents =
            "[console_scripts]\nfoo = foo:main\n\n[pytest11]\ndjango = pytest_django.plugin\n";
        assert_eq!(
            entry_point_names(contents, "pytest11"),
            vec!["django".to_owned()]
        );
    }

    #[test]
    fn top_level_txt_skips_blank_and_private_names() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let dist_info = dir.path().join("demo-1.0.dist-info");
        std::fs::create_dir(&dist_info).expect("mkdir");
        std::fs::write(dist_info.join("METADATA"), "Name: demo\n").expect("write");
        std::fs::write(
            dist_info.join("top_level.txt"),
            "demo_pkg\n\n_demo_native\n",
        )
        .expect("write");
        let mut index = VenvIndex::default();
        merge_distribution(&dist_info, Layout::DistInfo, &mut index).expect("valid name");
        assert_eq!(
            index.imports,
            BTreeMap::from([("demo_pkg".to_owned(), vec!["demo".to_owned()])])
        );
    }

    #[test]
    fn entry_points_record_binaries_and_only_pytest11_plugins() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        for (name, entry_points) in [
            ("tool", "[console_scripts]\ntool-cli = tool.main:main\n"),
            ("plug", "[pytest11]\nplug = plug.plugin\n"),
        ] {
            let dist_info = dir.path().join(format!("{name}-1.0.dist-info"));
            std::fs::create_dir(&dist_info).expect("mkdir");
            std::fs::write(dist_info.join("METADATA"), format!("Name: {name}\n")).expect("write");
            std::fs::write(dist_info.join("entry_points.txt"), entry_points).expect("write");
        }
        let mut index = VenvIndex::default();
        for name in ["tool", "plug"] {
            merge_distribution(
                &dir.path().join(format!("{name}-1.0.dist-info")),
                Layout::DistInfo,
                &mut index,
            )
            .expect("valid name");
        }
        assert_eq!(
            index.binaries,
            BTreeMap::from([("tool-cli".to_owned(), "tool".to_owned())])
        );
        assert_eq!(index.pytest_plugins, BTreeSet::from(["plug".to_owned()]));
    }

    #[test]
    fn record_skips_dist_info_and_parent_relative_paths() {
        assert_eq!(
            record_roots("demo-1.0.dist-info/METADATA,,\n../../bin/demo,,\n./demo.py,,\n"),
            vec!["demo".to_owned()]
        );
    }

    #[test]
    fn entry_points_skip_comments_and_other_groups() {
        let contents =
            "[console_scripts]\n# old = demo:old\nnew = demo:main\n[gui_scripts]\ngui = demo:gui\n";
        assert_eq!(
            entry_point_names(contents, "console_scripts"),
            vec!["new".to_owned()]
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn dist_info_name_drops_the_version(
                name in "[A-Za-z0-9][A-Za-z0-9._-]{0,10}",
                version in "[0-9][0-9a-z.+]{0,8}",
            ) {
                let file_name = format!("{name}-{version}.dist-info");
                prop_assert_eq!(distribution_dir_name(&file_name), Some(name.as_str()));
            }

            #[test]
            fn record_path_yields_its_first_segment(
                first in "[a-z][a-z0-9_]{0,6}",
                rest in prop::collection::vec("[a-z][a-z0-9_.]{0,6}", 0..3),
                dot_slash in any::<bool>(),
                module_file in any::<bool>(),
            ) {
                // A lone file in site-packages is a module only by its suffix.
                let expected = (module_file || !rest.is_empty()).then(|| first.clone());
                let mut path = if module_file && rest.is_empty() {
                    format!("{first}.py")
                } else {
                    std::iter::once(first).chain(rest).collect::<Vec<_>>().join("/")
                };
                if dot_slash {
                    path.insert_str(0, "./");
                }
                prop_assert_eq!(installed_module(&path).map(|module| module.root), expected);
            }
        }
    }
}
