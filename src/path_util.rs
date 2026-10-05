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

            #[test]
            fn normalize_rel_path_only_rewrites_separators(raw in "[a-z/\\\\._-]{0,40}") {
                let normalized = normalize_rel_path(Path::new(&raw));
                prop_assert_eq!(&normalized, &raw.replace('\\', "/"));
                prop_assert_eq!(normalize_rel_path(Path::new(&normalized)), normalized);
            }

            #[test]
            fn join_rel_treats_empty_base_as_root(rel in "[a-z/._]{0,20}") {
                prop_assert_eq!(join_rel("", &rel), rel);
            }

            #[test]
            fn join_rel_concatenates_segments(
                base in rel_segments(),
                rel in rel_segments(),
            ) {
                let joined = join_rel(&base.join("/"), &rel.join("/"));
                let expected: Vec<&str> = base.iter().chain(&rel).map(String::as_str).collect();
                prop_assert_eq!(joined.split('/').collect::<Vec<_>>(), expected);
            }

            #[test]
            fn join_rel_is_associative(
                a in prop::option::of(rel_segments()),
                b in rel_segments(),
                c in rel_segments(),
            ) {
                let a = a.map(|segments| segments.join("/")).unwrap_or_default();
                let (b, c) = (b.join("/"), c.join("/"));
                prop_assert_eq!(
                    join_rel(&join_rel(&a, &b), &c),
                    join_rel(&a, &join_rel(&b, &c))
                );
            }
        }
    }
}
