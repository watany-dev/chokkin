//! Cross-module path normalization helpers.

use std::path::Path;

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

    mod props {
        use super::*;

        proptest! {
            #[test]
            fn normalize_rel_path_strips_backslashes(raw in "\\PC{0,60}") {
                let normalized = normalize_rel_path(Path::new(&raw));
                prop_assert!(!normalized.contains('\\'));
            }

            #[test]
            fn normalize_rel_path_is_idempotent(raw in "\\PC{0,60}") {
                let once = normalize_rel_path(Path::new(&raw));
                prop_assert_eq!(normalize_rel_path(Path::new(&once)), once);
            }
        }
    }
}
