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

/// `path` relative to `root` in forward-slash form; paths outside `root` are
/// kept whole.
#[must_use]
pub fn rel_to_root(root: &Path, path: &Path) -> String {
    normalize_rel_path(path.strip_prefix(root).unwrap_or(path))
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

    #[test]
    fn rel_to_root_strips_root_prefix() {
        let root = Path::new("/proj");
        assert_eq!(rel_to_root(root, Path::new("/proj/a/b.txt")), "a/b.txt");
    }

    #[test]
    fn rel_to_root_falls_back_to_full_path_outside_root() {
        let root = Path::new("/proj");
        assert_eq!(rel_to_root(root, Path::new("/other/x.txt")), "/other/x.txt");
    }

    mod props {
        use super::*;
        use std::path::PathBuf;

        fn rel_segments() -> impl Strategy<Value = Vec<String>> {
            prop::collection::vec("[a-z][a-z0-9_.]{0,10}", 1..4)
        }

        proptest! {
            #[test]
            fn rel_to_root_roundtrips_paths_under_root(segments in rel_segments()) {
                let root = PathBuf::from("/proj");
                let mut path = root.clone();
                for segment in &segments {
                    path.push(segment);
                }

                prop_assert_eq!(rel_to_root(&root, &path), segments.join("/"));
            }

            #[test]
            fn rel_to_root_never_yields_backslashes(
                root in "[a-zA-Z0-9/_.-]{0,30}",
                path in "\\PC{0,60}",
            ) {
                let result = rel_to_root(Path::new(&root), Path::new(&path));
                prop_assert!(!result.contains('\\'));
            }

            #[test]
            fn normalize_rel_path_strips_backslashes(raw in "\\PC{0,60}") {
                let normalized = normalize_rel_path(Path::new(&raw));
                prop_assert!(!normalized.contains('\\'));
            }
        }
    }
}
