//! Atomic file writes with permission preservation.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use super::error::FixError;

fn manifest_name<'a>(path: &'a Path, fallback: &'static str) -> &'a str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(fallback)
}

const BOM: &str = "\u{feff}";

/// A manifest read for editing: its display name, its text without a leading
/// UTF-8 BOM (analysis drops it too, so the first line still matches), and
/// whether that BOM is written back.
pub(super) struct ManifestText<'a> {
    pub(super) rel: &'a str,
    pub(super) contents: String,
    pub(super) bom: bool,
}

/// Read a manifest, returning its display name alongside the contents.
pub(super) fn read_manifest<'a>(
    path: &'a Path,
    fallback: &'static str,
) -> Result<ManifestText<'a>, FixError> {
    let rel = manifest_name(path, fallback);
    let mut contents = fs::read_to_string(path).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;
    let bom = contents.starts_with(BOM);
    if bom {
        contents.drain(..BOM.len());
    }
    Ok(ManifestText { rel, contents, bom })
}

/// Atomically write a manifest, reporting failures under its display name `rel`
/// and restoring the BOM `read_manifest` took off.
pub(super) fn write_manifest(
    path: &Path,
    rel: &str,
    bom: bool,
    text: &str,
) -> Result<(), FixError> {
    let bytes = [if bom { BOM } else { "" }, text].concat();
    atomic_write(path, bytes.as_bytes(), true).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })
}

/// Write `bytes` to `path` atomically via a same-directory temp file and rename,
/// keeping the existing file's permissions. `sync` fsyncs before the rename.
///
/// # Errors
///
/// Returns the underlying I/O error when the temp file, write, or rename fails.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8], sync: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing parent directory"))?;
    let original_metadata = fs::metadata(path).ok();
    let (temp_path, mut file) = create_temp_in(parent)?;
    let result = (|| {
        file.write_all(bytes)?;
        if sync {
            file.sync_all()?;
        }
        if let Some(metadata) = original_metadata {
            file.set_permissions(metadata.permissions())?;
        }
        drop(file);
        fs::rename(&temp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_name(seq: u64) -> String {
    format!(".chokkin-{}-{seq}.tmp", process::id())
}

/// Create a fresh `.chokkin-*.tmp` file in `dir`; `create_new` guarantees we
/// never clobber a file another process (or a stale run) left behind.
fn create_temp_in(dir: &Path) -> io::Result<(PathBuf, fs::File)> {
    let mut last_error = None;
    for _ in 0..16 {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = dir.join(temp_name(seq));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("could not create temp file")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn assert_no_temp_files(dir: &Path) {
        let leftovers: Vec<_> = fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".chokkin-"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
    }

    #[test]
    fn atomic_write_replaces_contents() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("pyproject.toml");
        fs::write(&path, "old").expect("write");
        atomic_write(&path, b"new", true).expect("atomic write");
        assert_eq!(fs::read_to_string(&path).expect("read"), "new");
        assert_no_temp_files(dir.path());
    }

    #[test]
    fn atomic_write_removes_temp_file_when_rename_fails() {
        let dir = TempDir::new().expect("tempdir");
        let target = dir.path().join("occupied");
        fs::create_dir(&target).expect("mkdir");
        fs::write(target.join("child"), "x").expect("write");

        assert!(atomic_write(&target, b"new", false).is_err());
        assert_no_temp_files(dir.path());
    }

    #[test]
    fn create_temp_in_skips_a_stale_temp_file() {
        let dir = TempDir::new().expect("tempdir");
        let next = TEMP_SEQ.load(Ordering::Relaxed);
        // Fewer than the 16 attempts, and wide enough that other tests bumping
        // TEMP_SEQ in the meantime still leave a stale file in the way.
        for seq in next..next + 8 {
            fs::write(dir.path().join(temp_name(seq)), "stale").expect("write");
        }
        let (path, _file) = create_temp_in(dir.path()).expect("temp file");
        assert_eq!(fs::read_to_string(&path).expect("read"), "");
        assert_eq!(fs::read_dir(dir.path()).expect("read_dir").count(), 9);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("pyproject.toml");
        fs::write(&path, "old").expect("write");
        let mut permissions = fs::metadata(&path).expect("meta").permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&path, permissions).expect("chmod");

        atomic_write(&path, b"new", true).expect("atomic write");

        let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
