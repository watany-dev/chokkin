//! Cross-module path normalization helpers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Render a path without Windows' verbatim prefix.
#[must_use]
pub(crate) fn display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_owned()
}

/// Normalize a root-relative path to forward-slash form.
#[must_use]
pub(crate) fn normalize_rel_path(path: &Path) -> String {
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
pub(crate) fn rel_to_root(root: &Path, path: &Path) -> String {
    normalize_rel_path(path.strip_prefix(root).unwrap_or(path))
}

/// Whether `dir` holds a file named exactly `name`. `dir.join(name).is_file()`
/// alone also finds `Requirements.txt` for `requirements.txt` on a
/// case-insensitive file system (macOS, Windows), where the tools that read
/// the file on Linux would not (#1163); the listing keeps each entry's own
/// spelling.
#[must_use]
pub(crate) fn is_file_named(dir: &Path, name: &str) -> bool {
    dir.join(name).is_file() && has_entry_named(dir, name)
}

/// Whether the listing of `dir` holds an entry spelled exactly `name`.
#[must_use]
pub(crate) fn has_entry_named(dir: &Path, name: &str) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().as_os_str() == name)
    })
}

/// Whether a directory above the module at root-relative `path` holds an
/// `__init__.py` that `is_init` accepts: importing that package does not by
/// itself load `path` (#802).
#[must_use]
pub(crate) fn is_submodule(path: &str, is_init: impl Fn(&str) -> bool) -> bool {
    let mut dir = path.strip_suffix("/__init__.py").unwrap_or(path);
    while let Some((parent, _)) = dir.rsplit_once('/') {
        if is_init(&format!("{parent}/__init__.py")) {
            return true;
        }
        dir = parent;
    }
    false
}

/// Join root-relative `/`-separated paths, where `""` is the root.
#[must_use]
pub(crate) fn join_rel(base: &str, rel: &str) -> String {
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
/// a `..` sits below a missing directory and so cannot be resolved, or the
/// ancestor is a dangling symlink.
pub(crate) fn resolve_under_root(
    canonical_root: &Path,
    path: &Path,
) -> io::Result<Option<PathBuf>> {
    // Components drop a `.`, which would make the name before it look up as
    // a directory, so a dangling link before a trailing `/.` looks missing.
    let path: PathBuf = path.components().collect();
    let mut ancestor = path.as_path();
    let mut missing = Vec::new();
    // A dangling symlink is no missing name: a write through it creates its
    // target, so it is canonicalized (and fails) like any existing link.
    while fs::symlink_metadata(ancestor).is_err() {
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
    fn is_file_named_matches_the_spelling_on_disk() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("Requirements.txt"), "").expect("write");
        fs::create_dir(temp.path().join("setup.py")).expect("create dir");
        assert!(is_file_named(temp.path(), "Requirements.txt"));
        assert!(!is_file_named(temp.path(), "requirements.txt"));
        // A directory under the name is no file.
        assert!(!is_file_named(temp.path(), "setup.py"));
        assert!(!is_file_named(&temp.path().join("missing"), "setup.py"));
    }

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

    /// A write through a dangling link creates its target wherever it points.
    #[cfg(unix)]
    #[test]
    fn resolve_under_root_refuses_a_dangling_symlink() {
        let (_dir, root) = canonical_tempdir();
        let (_outside, outside) = canonical_tempdir();
        std::os::unix::fs::symlink(outside.join("gone"), root.join("link")).expect("symlink");
        resolve_under_root(&root, &root.join("link")).expect_err("dangling link");
        resolve_under_root(&root, &root.join("link").join("x")).expect_err("dangling link");
        resolve_under_root(&root, &root.join("link/.")).expect_err("dangling link");
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
