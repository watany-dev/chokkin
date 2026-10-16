//! Lockfile discovery and dispatch to the per-format readers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml::Value;

use crate::path_util::rel_to_root;

use super::error::ManifestError;
use super::pdm_lock::extract_pdm_lock;
use super::pep508_util::normalize_distribution_name;
use super::poetry_lock::extract_poetry_lock;
use super::pylock::extract_pylock;
use super::types::{LockfileGraph, LockfileKind, LockfileSource};
use super::util::read_to_string;
use super::uv_lock::extract_uv_lock;
use super::warnings::ManifestWarning;

/// Lockfiles beyond either limit are skipped with a warning instead of read
/// (#972). The corpus's largest is airflow's `uv.lock`: 3.2 MB, 960 packages.
const MAX_LOCKFILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_LOCKFILE_PACKAGES: usize = 20_000;

/// Existing lockfiles directly under `root`, highest priority first
/// (`uv.lock` > `pylock.toml` > `pylock.<name>.toml` > `poetry.lock` > `pdm.lock`).
pub(crate) fn lockfile_candidates(root: &Path) -> Vec<(LockfileKind, PathBuf)> {
    let mut found = Vec::new();
    push_if_file(&mut found, LockfileKind::Uv, root.join("uv.lock"));
    push_if_file(&mut found, LockfileKind::Pylock, root.join("pylock.toml"));
    found.extend(
        named_pylock_files(root)
            .into_iter()
            .map(|path| (LockfileKind::Pylock, path)),
    );
    push_if_file(&mut found, LockfileKind::Poetry, root.join("poetry.lock"));
    push_if_file(&mut found, LockfileKind::Pdm, root.join("pdm.lock"));
    found
}

/// Read the highest-priority usable lockfile under `root`, if any. One over
/// the size limits, or one that lists no package (textual's 52-byte stub
/// `uv.lock` beside a full `poetry.lock`, #976), is skipped with a warning
/// for the next candidate. With none left, the lock counts as absent:
/// CHK003 then stands for CHK004 too, both asking for the same declaration,
/// so nothing is lowered.
pub(super) fn extract_lockfile(
    root: &Path,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<Option<(LockfileSource, LockfileGraph)>, ManifestError> {
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut used = None;
    for (kind, path) in lockfile_candidates(root) {
        let file = rel_to_root(root, &path);
        match read_candidate(kind, &path)? {
            Ok(graph) => {
                used = Some((LockfileSource { kind, path: file }, graph));
                break;
            },
            Err(cause) => skipped.push((file, cause)),
        }
    }
    let consequence = used.as_ref().map_or_else(
        || "transitive dependencies (CHK004) are not checked".to_owned(),
        |(source, _)| format!("using `{}` instead", source.path),
    );
    warnings.extend(
        skipped
            .into_iter()
            .map(|(file, cause)| ManifestWarning::LockfileSkipped {
                file,
                reason: format!("{cause}; {consequence}"),
            }),
    );
    Ok(used)
}

/// The lock's graph, or why it is not used.
fn read_candidate(
    kind: LockfileKind,
    path: &Path,
) -> Result<Result<LockfileGraph, String>, ManifestError> {
    let bytes = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
    // Counting the table headers first spares parsing a lock already too
    // large; the parsed count still catches packages written another way.
    let headers = std::fs::read_to_string(path)
        .ok()
        .map(|contents| package_headers(&contents));
    if let Some(cause) = over_limit(bytes, headers) {
        return Ok(Err(cause));
    }
    let graph = match kind {
        LockfileKind::Uv => extract_uv_lock(path)?,
        LockfileKind::Pylock => extract_pylock(path)?,
        LockfileKind::Poetry => extract_poetry_lock(path)?,
        LockfileKind::Pdm => extract_pdm_lock(path)?,
    };
    if let Some(cause) = over_limit(bytes, Some(graph.edges.len())) {
        return Ok(Err(cause));
    }
    // A lock always lists the project or its dependencies, so an empty one is
    // a placeholder, not a project without dependencies.
    if graph.edges.is_empty() {
        return Ok(Err("it lists no packages".to_owned()));
    }
    Ok(Ok(graph))
}

/// `[[package]]` (uv, Poetry, PDM) and `[[packages]]` (PEP 751) headers.
fn package_headers(contents: &str) -> usize {
    contents
        .lines()
        .filter(|line| matches!(line.trim(), "[[package]]" | "[[packages]]"))
        .count()
}

/// Why a lockfile of `bytes` and `packages` is too large.
fn over_limit(bytes: u64, packages: Option<usize>) -> Option<String> {
    if bytes > MAX_LOCKFILE_BYTES {
        return Some(format!(
            "{bytes} bytes exceed the {MAX_LOCKFILE_BYTES}-byte limit"
        ));
    }
    packages
        .filter(|&packages| packages > MAX_LOCKFILE_PACKAGES)
        .map(|packages| {
            format!("{packages} packages exceed the {MAX_LOCKFILE_PACKAGES}-package limit")
        })
}

fn push_if_file(found: &mut Vec<(LockfileKind, PathBuf)>, kind: LockfileKind, path: PathBuf) {
    if path.is_file() {
        found.push((kind, path));
    }
}

/// PEP 751 named lockfiles, sorted so the pick is stable across platforms.
fn named_pylock_files(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_named_pylock)
                && path.is_file()
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn is_named_pylock(file_name: &str) -> bool {
    file_name
        .strip_prefix("pylock.")
        .and_then(|rest| rest.strip_suffix(".toml"))
        .is_some_and(|name| !name.is_empty() && !name.contains('.'))
}

/// Parse a non-uv lockfile as a TOML table.
pub(super) fn read_lock_table(path: &Path) -> Result<toml::Table, ManifestError> {
    let contents = read_to_string(path)?;
    toml::from_str(&contents).map_err(|error| ManifestError::InvalidLockfile {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

/// Tables of the array `key` in `table`, skipping non-table entries.
pub(super) fn array_tables<'a>(
    table: &'a toml::Table,
    key: &str,
) -> impl Iterator<Item = &'a toml::Table> + use<'a> {
    table
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_table)
}

/// Add `name -> deps`, merging with an earlier entry for the same package
/// (multiple versions, markers, or PDM extras variants).
pub(super) fn merge_package(
    edges: &mut BTreeMap<String, Vec<String>>,
    name: &str,
    deps: impl IntoIterator<Item = String>,
) {
    let entry = edges.entry(normalize_distribution_name(name)).or_default();
    for dep in deps {
        if !entry.contains(&dep) {
            entry.push(dep);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_follow_priority_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        for name in [
            "pdm.lock",
            "poetry.lock",
            "pylock.dev.toml",
            "pylock.toml",
            "uv.lock",
        ] {
            std::fs::write(temp.path().join(name), "").expect("write");
        }
        let kinds = lockfile_candidates(temp.path())
            .into_iter()
            .map(|(kind, path)| {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned();
                (kind, name)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                (LockfileKind::Uv, "uv.lock".to_owned()),
                (LockfileKind::Pylock, "pylock.toml".to_owned()),
                (LockfileKind::Pylock, "pylock.dev.toml".to_owned()),
                (LockfileKind::Poetry, "poetry.lock".to_owned()),
                (LockfileKind::Pdm, "pdm.lock".to_owned()),
            ]
        );
    }

    #[test]
    fn named_pylock_requires_a_single_name_segment() {
        assert!(is_named_pylock("pylock.dev.toml"));
        assert!(!is_named_pylock("pylock.toml"));
        assert!(!is_named_pylock("pylock..toml"));
        assert!(!is_named_pylock("pylock.a.b.toml"));
        assert!(!is_named_pylock("xpylock.dev.toml"));
    }

    #[test]
    fn extract_lockfile_reports_kind_and_relative_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("pdm.lock"),
            "[[package]]\nname = \"requests\"\ndependencies = [\"urllib3<3\"]\n",
        )
        .expect("write");
        let (source, graph) = extract_lockfile(temp.path(), &mut Vec::new())
            .expect("valid pdm.lock")
            .expect("lockfile present");
        assert_eq!(source.kind, LockfileKind::Pdm);
        assert_eq!(source.path, "pdm.lock");
        assert_eq!(
            graph.edges.get("requests"),
            Some(&vec!["urllib3".to_owned()])
        );
    }

    #[test]
    fn no_lockfile_yields_none() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(
            extract_lockfile(temp.path(), &mut Vec::new())
                .expect("ok")
                .is_none()
        );
    }

    #[test]
    fn limits_name_the_exceeded_bound() {
        assert_eq!(
            over_limit(MAX_LOCKFILE_BYTES, Some(MAX_LOCKFILE_PACKAGES)),
            None
        );
        let bytes = over_limit(MAX_LOCKFILE_BYTES + 1, None).expect("over bytes");
        assert!(bytes.contains("byte limit"), "{bytes}");
        let packages = over_limit(1, Some(MAX_LOCKFILE_PACKAGES + 1)).expect("over packages");
        assert!(packages.contains("20001 packages"), "{packages}");
    }

    #[test]
    fn package_headers_count_every_lock_format() {
        let contents =
            "[[package]]\nname = \"a\"\n  [[package]]\n[[packages]]\n[package.metadata]\n";
        assert_eq!(package_headers(contents), 3);
    }

    #[test]
    fn lockfile_over_the_package_limit_is_skipped_with_a_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut lock = String::new();
        for i in 0..=MAX_LOCKFILE_PACKAGES {
            lock.push_str("[[package]]\nname = \"pkg-");
            lock.push_str(&i.to_string());
            lock.push_str("\"\n");
        }
        std::fs::write(temp.path().join("pdm.lock"), lock).expect("write");
        let mut warnings = Vec::new();
        assert!(
            extract_lockfile(temp.path(), &mut warnings)
                .expect("ok")
                .is_none()
        );
        assert!(
            matches!(
                warnings.as_slice(),
                [ManifestWarning::LockfileSkipped { file, reason }]
                    if file == "pdm.lock" && reason.contains("CHK004")
            ),
            "{warnings:?}"
        );
    }

    /// textual ships a stub `uv.lock` beside its `poetry.lock` (#976).
    #[test]
    fn a_lockfile_without_packages_falls_back_to_the_next_with_a_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("uv.lock"),
            "version = 1\nrevision = 3\nrequires-python = \">=3.11\"\n",
        )
        .expect("write");
        std::fs::write(
            temp.path().join("poetry.lock"),
            "[[package]]\nname = \"rich\"\nversion = \"13.0\"\n\n[package.dependencies]\nmarkdown-it-py = \">=2\"\n",
        )
        .expect("write");
        let mut warnings = Vec::new();
        let (source, graph) = extract_lockfile(temp.path(), &mut warnings)
            .expect("ok")
            .expect("poetry.lock used");
        assert_eq!(source.kind, LockfileKind::Poetry);
        assert_eq!(
            graph.edges.get("rich"),
            Some(&vec!["markdown-it-py".to_owned()])
        );
        assert_eq!(
            warnings,
            [ManifestWarning::LockfileSkipped {
                file: "uv.lock".to_owned(),
                reason: "it lists no packages; using `poetry.lock` instead".to_owned(),
            }]
        );
    }

    #[test]
    fn a_lone_empty_lockfile_counts_as_absent_with_a_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("uv.lock"), "version = 1\n").expect("write");
        let mut warnings = Vec::new();
        assert!(
            extract_lockfile(temp.path(), &mut warnings)
                .expect("ok")
                .is_none()
        );
        assert_eq!(
            warnings,
            [ManifestWarning::LockfileSkipped {
                file: "uv.lock".to_owned(),
                reason: "it lists no packages; transitive dependencies (CHK004) are not checked"
                    .to_owned(),
            }]
        );
    }
}
