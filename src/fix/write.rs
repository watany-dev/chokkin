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

/// Read a manifest, returning its display name alongside the contents.
pub(super) fn read_manifest<'a>(
    path: &'a Path,
    fallback: &'static str,
) -> Result<(&'a str, String), FixError> {
    let rel = manifest_name(path, fallback);
    let contents = fs::read_to_string(path).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;
    Ok((rel, contents))
}

/// Write `bytes` to `path` atomically via a same-directory temp file and rename,
/// keeping the existing file's permissions. `sync` fsyncs before the rename.
///
/// # Errors
///
/// Returns the underlying I/O error when the temp file, write, or rename fails.
pub fn atomic_write(path: &Path, bytes: &[u8], sync: bool) -> io::Result<()> {
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

/// Create a fresh `.chokkin-*.tmp` file in `dir`; `create_new` guarantees we
/// never clobber a file another process (or a stale run) left behind.
fn create_temp_in(dir: &Path) -> io::Result<(PathBuf, fs::File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut last_error = None;
    for _ in 0..16 {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!(".chokkin-{}-{seq}.tmp", process::id());
        let candidate = dir.join(name);
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
