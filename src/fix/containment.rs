//! Project-root path containment checks for fix writes.

use std::path::{Component, Path, PathBuf};

use super::error::FixError;

/// Resolve `file` under `root` and ensure the result stays inside the project root.
///
/// # Errors
///
/// Returns [`FixError::Unsupported`] when `file` escapes the project root.
pub fn resolve_contained_path(root: &Path, file: &str) -> Result<PathBuf, FixError> {
    if Path::new(file).is_absolute() {
        return Err(FixError::Unsupported {
            detail: format!("absolute fix target `{file}` is not allowed"),
        });
    }
    for component in Path::new(file).components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(FixError::Unsupported {
                detail: format!("fix target `{file}` escapes the project root"),
            });
        }
    }

    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let joined = root.join(file);
    let canonical_joined = if let Ok(path) = std::fs::canonicalize(&joined) {
        path
    } else {
        let parent = joined.parent().ok_or_else(|| FixError::Unsupported {
            detail: format!("fix target `{file}` has no parent directory"),
        })?;
        let canonical_parent =
            std::fs::canonicalize(parent).map_err(|_| FixError::Unsupported {
                detail: format!("fix target parent for `{file}` cannot be resolved"),
            })?;
        let name = joined.file_name().ok_or_else(|| FixError::Unsupported {
            detail: format!("fix target `{file}` has no file name"),
        })?;
        canonical_parent.join(name)
    };
    if !canonical_joined.starts_with(&canonical_root) {
        return Err(FixError::Unsupported {
            detail: format!("fix target `{file}` resolves outside the project root"),
        });
    }
    Ok(canonical_joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_missing_file_under_existing_root_parent() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let resolved = resolve_contained_path(dir.path(), "pyproject.toml").expect("resolve");
        let expected = dir
            .path()
            .canonicalize()
            .expect("canonical tempdir")
            .join("pyproject.toml");

        assert_eq!(resolved, expected);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_missing_file_under_symlinked_outside_parent() {
        use std::os::unix::fs::symlink;

        let root = tempfile::TempDir::new().expect("root");
        let outside = tempfile::TempDir::new().expect("outside");
        symlink(outside.path(), root.path().join("linked")).expect("symlink");

        let error = resolve_contained_path(root.path(), "linked/pyproject.toml")
            .expect_err("symlink target should escape root");

        assert!(matches!(error, FixError::Unsupported { .. }));
        assert!(
            error
                .to_string()
                .contains("resolves outside the project root")
        );
    }
}
