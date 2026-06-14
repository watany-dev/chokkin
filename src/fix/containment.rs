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

    let joined = root.join(file);
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canonical_joined = std::fs::canonicalize(&joined).unwrap_or(joined);
    if !canonical_joined.starts_with(&canonical_root) {
        return Err(FixError::Unsupported {
            detail: format!("fix target `{file}` resolves outside the project root"),
        });
    }
    Ok(canonical_joined)
}
