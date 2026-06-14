//! Atomic manifest file writes with permission preservation.

use std::fs;
use std::io::Write;
use std::path::Path;

use super::error::FixError;

/// Write `contents` to `path` atomically via a same-directory temp file and rename.
///
/// # Errors
///
/// Returns [`FixError::Io`] when the temp file or final rename fails.
pub fn atomic_write(path: &Path, contents: &str) -> Result<(), FixError> {
    let rel = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("manifest");

    let parent = path.parent().ok_or_else(|| FixError::Io {
        path: rel.to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing parent directory"),
    })?;

    let original_metadata = fs::metadata(path).ok();

    let mut temp = tempfile::Builder::new()
        .prefix(".yokei-")
        .tempfile_in(parent)
        .map_err(|source| FixError::Io {
            path: rel.to_owned(),
            source,
        })?;

    temp.write_all(contents.as_bytes())
        .map_err(|source| FixError::Io {
            path: rel.to_owned(),
            source,
        })?;
    temp.as_file().sync_all().map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;

    if let Some(metadata) = &original_metadata {
        let permissions = metadata.permissions();
        temp.as_file()
            .set_permissions(permissions)
            .map_err(|source| FixError::Io {
                path: rel.to_owned(),
                source,
            })?;
    }

    temp.persist(path).map_err(|error| FixError::Io {
        path: rel.to_owned(),
        source: error.error,
    })?;

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
        atomic_write(&path, "new").expect("atomic write");
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

        atomic_write(&path, "new").expect("atomic write");

        let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
