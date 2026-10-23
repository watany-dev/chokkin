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
        if !file_name.ends_with(".dist-info") {
            continue;
        }
        if let Err(name) = merge_dist_info(&path, &mut index) {
            warnings.push(ResolveWarning::VenvDistributionIgnored {
                path: rel_to_root(root, &path),
                name,
            });
        }
    }

    for imports in index.imports.values_mut() {
        imports.sort();
        imports.dedup();
    }
    Ok(index)
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
    if candidates.len() > 1 {
        let configured = configured_version(venv);
        let matching: Vec<_> = candidates
            .iter()
            .filter(|(version, _)| version.is_some() && *version == configured)
            .cloned()
            .collect();
        if !matching.is_empty() {
            candidates = matching;
        }
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
fn merge_dist_info(dist_info: &Path, index: &mut VenvIndex) -> Result<(), String> {
    let metadata_path = dist_info.join("METADATA");
    let Ok(contents) = fs::read_to_string(&metadata_path) else {
        return Ok(());
    };
    let metadata = parse_metadata(&contents);
    let name = metadata.name.clone().or_else(|| {
        dist_info
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(dist_info_name)
            .map(str::to_owned)
    });
    let Some(name) = name else {
        return Ok(());
    };
    if !is_project_name(&name) {
        return Err(name);
    }
    let distribution = normalize_distribution_name(&name);

    let top_level_path = dist_info.join("top_level.txt");
    if let Ok(top_level) = fs::read_to_string(&top_level_path) {
        for line in top_level.lines() {
            let import = line.trim();
            if !import.is_empty() && !import.starts_with('_') {
                push_import(index, import, &distribution);
            }
        }
    }

    for import in metadata.import_names {
        push_import(index, &import, &distribution);
    }
    for import in metadata.import_namespaces {
        push_import(index, &import, &distribution);
    }

    let record_path = dist_info.join("RECORD");
    for import in imports_from_record(&record_path) {
        push_import(index, &import, &distribution);
    }

    merge_entry_points(&dist_info.join("entry_points.txt"), &distribution, index);
    Ok(())
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

fn dist_info_name(file_name: &str) -> Option<&str> {
    let stem = file_name.strip_suffix(".dist-info")?;
    let (name, _version) = stem.rsplit_once('-')?;
    Some(name)
}

fn imports_from_record(record_path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(record_path) else {
        return Vec::new();
    };
    let mut imports = std::collections::BTreeSet::new();
    for line in contents.lines() {
        let Some(path) = line.split(',').next() else {
            continue;
        };
        let path = path.trim();
        if path.is_empty() || path.contains(".dist-info/") {
            continue;
        }
        if let Some(import) = top_level_import_from_record_path(path)
            && !import.starts_with('_')
        {
            imports.insert(import);
        }
    }
    imports.into_iter().collect()
}

fn top_level_import_from_record_path(path: &str) -> Option<String> {
    let path = path.trim().trim_start_matches("./");
    if path.is_empty() || path.starts_with('.') {
        return None;
    }
    let first = path.split('/').next()?;
    Some(first.strip_suffix(".py").unwrap_or(first).to_owned())
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

    #[test]
    fn parses_dist_info_directory_name() {
        assert_eq!(dist_info_name("PyYAML-6.0.1.dist-info"), Some("PyYAML"));
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
                merge_dist_info(&dist_info, &mut index).is_err(),
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
        let dir = tempfile::TempDir::new().expect("tempdir");
        let record = dir.path().join("RECORD");
        std::fs::write(
            &record,
            "yaml/__init__.py,sha256=abc,100\nyaml/loader.py,sha256=def,200\n",
        )
        .expect("write");
        assert_eq!(imports_from_record(&record), vec!["yaml".to_owned()]);
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
        merge_dist_info(&dist_info, &mut index).expect("valid name");
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
            merge_dist_info(
                &dir.path().join(format!("{name}-1.0.dist-info")),
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
        let dir = tempfile::TempDir::new().expect("tempdir");
        let record = dir.path().join("RECORD");
        std::fs::write(
            &record,
            "demo-1.0.dist-info/METADATA,,\n../../bin/demo,,\n./demo.py,,\n",
        )
        .expect("write");
        assert_eq!(imports_from_record(&record), vec!["demo".to_owned()]);
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
                prop_assert_eq!(dist_info_name(&file_name), Some(name.as_str()));
            }

            #[test]
            fn record_path_yields_its_first_segment(
                first in "[a-z][a-z0-9_]{0,6}",
                rest in prop::collection::vec("[a-z][a-z0-9_.]{0,6}", 0..3),
                dot_slash in any::<bool>(),
                module_file in any::<bool>(),
            ) {
                let mut path = if module_file && rest.is_empty() {
                    format!("{first}.py")
                } else {
                    std::iter::once(first.clone()).chain(rest).collect::<Vec<_>>().join("/")
                };
                if dot_slash {
                    path.insert_str(0, "./");
                }
                prop_assert_eq!(top_level_import_from_record_path(&path), Some(first));
            }
        }
    }
}
