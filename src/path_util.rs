//! Cross-module path normalization helpers.

use std::io;
use std::path::{Path, PathBuf};

/// Render a path without Windows' verbatim prefix.
#[must_use]
pub fn display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_owned()
}

/// Normalize a root-relative path to forward-slash form.
#[must_use]
pub fn normalize_rel_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if raw.contains('\\') {
        raw.replace('\\', "/")
    } else {
        raw.into_owned()
    }
}

/// Join root-relative `/`-separated paths, where `""` is the root.
#[must_use]
pub fn join_rel(base: &str, rel: &str) -> String {
    if base.is_empty() {
        rel.to_owned()
    } else {
        format!("{base}/{rel}")
    }
}

/// Resolve `path` through its deepest existing ancestor, following symlinks, and
/// return it only when the result stays under `canonical_root`.
///
/// # Errors
///
/// Returns the I/O error from canonicalizing the existing ancestor, including when
/// a `..` sits below a missing directory and so cannot be resolved.
pub fn resolve_under_root(canonical_root: &Path, path: &Path) -> io::Result<Option<PathBuf>> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            break;
        };
        missing.push(name);
        let Some(parent) = ancestor.parent() else {
            break;
        };
        ancestor = parent;
    }

    let mut resolved = ancestor.canonicalize()?;
    resolved.extend(missing.iter().rev());
    Ok(resolved.starts_with(canonical_root).then_some(resolved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn display_path_strips_windows_verbatim_disk_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\work\demo")),
            r"C:\work\demo"
        );
    }

    #[test]
    fn display_path_preserves_windows_unc_form() {
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\demo")),
            r"\\server\share\demo"
        );
    }

    #[test]
    fn display_path_leaves_normal_paths_unchanged() {
        assert_eq!(display_path(Path::new("/work/demo")), "/work/demo");
    }

    fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let canonical = dir.path().canonicalize().expect("canonical tempdir");
        (dir, canonical)
    }

    #[test]
    fn resolve_under_root_appends_missing_components() {
        let (_dir, root) = canonical_tempdir();
        let resolved = resolve_under_root(&root, &root.join("a").join("b.json")).expect("resolve");
        assert_eq!(resolved, Some(root.join("a").join("b.json")));
    }

    #[test]
    fn resolve_under_root_rejects_parent_dir_escape() {
        let (_dir, root) = canonical_tempdir();
        std::fs::create_dir(root.join("sub")).expect("mkdir");
        let escaped = root.join("sub").join("..").join("..").join("x.json");
        assert_eq!(resolve_under_root(&root, &escaped).expect("resolve"), None);
        // Unix cannot resolve `..` below a missing directory; Windows does it lexically.
        let missing_parent = root.join("missing").join("..").join("..").join("x.json");
        assert!(!matches!(
            resolve_under_root(&root, &missing_parent),
            Ok(Some(_))
        ));
    }

    #[test]
    fn resolve_under_root_rejects_absolute_path_outside_root() {
        let (_dir, root) = canonical_tempdir();
        let (_other, outside) = canonical_tempdir();
        assert_eq!(
            resolve_under_root(&root, &outside.join("x.json")).expect("resolve"),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_under_root_follows_symlink_out_of_root() {
        let (_dir, root) = canonical_tempdir();
        let (_other, outside) = canonical_tempdir();
        std::os::unix::fs::symlink(&outside, root.join("linked")).expect("symlink");
        let path = root.join("linked").join("x.json");
        assert_eq!(resolve_under_root(&root, &path).expect("resolve"), None);
    }

    mod props {
        use super::*;

        proptest! {
            #[test]
            fn normalize_rel_path_strips_backslashes(raw in "\\PC{0,60}") {
                let normalized = normalize_rel_path(Path::new(&raw));
                prop_assert!(!normalized.contains('\\'));
            }
        }
    }
}
