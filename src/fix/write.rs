//! Atomic file writes with permission preservation.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

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
    let mut temp = tempfile::Builder::new()
        .prefix(".chokkin-")
        .tempfile_in(parent)?;
    temp.write_all(bytes)?;
    if sync {
        temp.as_file().sync_all()?;
    }
    if let Some(metadata) = original_metadata {
        temp.as_file().set_permissions(metadata.permissions())?;
    }
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn atomic_write_replaces_contents() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("pyproject.toml");
        fs::write(&path, "old").expect("write");
        atomic_write(&path, b"new", true).expect("atomic write");
        assert_eq!(fs::read_to_string(&path).expect("read"), "new");
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
