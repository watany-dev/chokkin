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

/// Read the highest-priority lockfile under `root`, if any. One over the
/// size limits counts as absent: CHK003 then stands for CHK004 too, both
/// asking for the same declaration, so nothing is lowered.
pub(super) fn extract_lockfile(
    root: &Path,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<Option<(LockfileSource, LockfileGraph)>, ManifestError> {
    let Some((kind, path)) = lockfile_candidates(root).into_iter().next() else {
        return Ok(None);
    };
    let file = rel_to_root(root, &path);
    let bytes = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
    // Counting the table headers first spares parsing a lock already too
    // large; the parsed count still catches packages written another way.
    let headers = std::fs::read_to_string(&path)
        .ok()
        .map(|contents| package_headers(&contents));
    if let Some(reason) = over_limit(bytes, headers) {
        warnings.push(ManifestWarning::LockfileSkipped { file, reason });
        return Ok(None);
    }
    let graph = match kind {
        LockfileKind::Uv => extract_uv_lock(&path)?,
        LockfileKind::Pylock => extract_pylock(&path)?,
        LockfileKind::Poetry => extract_poetry_lock(&path)?,
        LockfileKind::Pdm => extract_pdm_lock(&path)?,
    };
    if let Some(reason) = over_limit(bytes, Some(graph.edges.len())) {
        warnings.push(ManifestWarning::LockfileSkipped { file, reason });
        return Ok(None);
    }
    let source = LockfileSource { kind, path: file };
    Ok(Some((source, graph)))
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
    const CONSEQUENCE: &str = "transitive dependencies (CHK004) are not checked";
    if bytes > MAX_LOCKFILE_BYTES {
        return Some(format!(
            "{bytes} bytes exceed the {MAX_LOCKFILE_BYTES}-byte limit; {CONSEQUENCE}"
        ));
    }
    packages
        .filter(|&packages| packages > MAX_LOCKFILE_PACKAGES)
        .map(|packages| {
            format!(
                "{packages} packages exceed the {MAX_LOCKFILE_PACKAGES}-package limit; {CONSEQUENCE}"
            )
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
}
