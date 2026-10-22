//! Project-root path containment checks for fix writes.

use std::path::{Component, Path, PathBuf};

use crate::path_util::resolve_under_root;

use super::error::FixError;

/// Resolve `file` under `root` and ensure the result stays inside the project root.
///
/// # Errors
///
/// Returns [`FixError::Unsupported`] when `file` escapes the project root.
pub(super) fn resolve_contained_path(root: &Path, file: &str) -> Result<PathBuf, FixError> {
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
    resolve_under_root(&canonical_root, &root.join(file))
        .map_err(|_| FixError::Unsupported {
            detail: format!("fix target parent for `{file}` cannot be resolved"),
        })?
        .ok_or_else(|| FixError::Unsupported {
            detail: format!("fix target `{file}` resolves outside the project root"),
        })
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

    #[test]
    fn rejects_absolute_target() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let absolute = dir.path().join("pyproject.toml");
        let file = absolute.to_str().expect("utf-8 tempdir");

        let error = resolve_contained_path(dir.path(), file).expect_err("absolute target");

        assert!(matches!(error, FixError::Unsupported { .. }));
        assert!(
            error
                .to_string()
                .contains(&format!("absolute fix target `{file}` is not allowed")),
            "{error}"
        );
    }

    /// A `..` below a missing directory leaves no existing ancestor to
    /// canonicalize. Windows resolves `..` lexically, so the root would exist.
    #[cfg(unix)]
    #[test]
    fn rejects_target_whose_parent_cannot_be_resolved() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let root = dir.path().join("missing").join("..");

        let error = resolve_contained_path(&root, "pyproject.toml").expect_err("unresolvable");

        assert!(
            error
                .to_string()
                .contains("fix target parent for `pyproject.toml` cannot be resolved"),
            "{error}"
        );
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

    /// `--fix` targets (#1105): whatever `..`, absolute prefix or symlink a
    /// target holds, a write to the resolved path lands under the root.
    #[cfg(unix)]
    mod props {
        use super::*;
        use proptest::prelude::*;
        use std::fs;
        use std::os::unix::fs::symlink;

        /// Names under the root: a directory and a file in it, links into
        /// and out of the root (live and dangling), and names that exist
        /// nowhere.
        const PARTS: [&str; 10] = [
            "dir",
            "file.toml",
            "in_link",
            "out_link",
            "dangling_in",
            "dangling_out",
            "new",
            "..",
            ".",
            "",
        ];

        /// Every file under `dir` with its contents, links not followed.
        fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
            let mut files = Vec::new();
            let mut stack = vec![dir.to_path_buf()];
            while let Some(current) = stack.pop() {
                for entry in fs::read_dir(&current).expect("read dir") {
                    let path = entry.expect("entry").path();
                    let meta = fs::symlink_metadata(&path).expect("metadata");
                    if meta.is_dir() {
                        stack.push(path);
                    } else if meta.is_file() {
                        let bytes = fs::read(&path).expect("read");
                        files.push((path, bytes));
                    }
                }
            }
            files.sort();
            files
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(crate::internals::proptest_cases(128)))]

            #[test]
            fn a_fix_target_never_writes_outside_the_root(
                parts in prop::collection::vec(prop::sample::select(PARTS.to_vec()), 1..4),
                absolute in any::<bool>(),
            ) {
                let base = tempfile::TempDir::new().expect("tempdir");
                let root = base.path().join("root");
                let outside = base.path().join("outside");
                fs::create_dir_all(root.join("dir")).expect("mkdir");
                fs::create_dir_all(&outside).expect("mkdir");
                fs::write(root.join("dir/file.toml"), "inside").expect("write");
                fs::write(outside.join("file.toml"), "outside").expect("write");
                symlink(root.join("dir"), root.join("in_link")).expect("symlink");
                symlink(&outside, root.join("out_link")).expect("symlink");
                symlink(root.join("gone"), root.join("dangling_in")).expect("symlink");
                symlink(outside.join("gone"), root.join("dangling_out")).expect("symlink");
                let before = snapshot(base.path());

                let file = parts.join("/");
                let file = if absolute { format!("{}/{file}", root.display()) } else { file };
                let Ok(path) = resolve_contained_path(&root, &file) else {
                    return Ok(());
                };
                let canonical_root = fs::canonicalize(&root).expect("canonical root");
                prop_assert!(path.starts_with(&canonical_root), "{file:?} -> {path:?}");
                // Write as `--fix` would and see where the bytes land.
                let _ = fs::OpenOptions::new().append(true).create(true).open(&path)
                    .and_then(|mut handle| std::io::Write::write_all(&mut handle, b"+"));
                let after = snapshot(base.path());
                let changed: Vec<&PathBuf> = after
                    .iter()
                    .filter(|entry| !before.contains(entry))
                    .map(|(path, _)| path)
                    .collect();
                for written in changed {
                    prop_assert!(written.starts_with(&root), "{file:?} wrote {written:?}");
                }
            }

            /// A plain relative target through live in-root links resolves.
            #[test]
            fn a_plain_target_inside_the_root_resolves(
                parts in prop::collection::vec(
                    prop::sample::select(vec!["dir", "in_link", "new", "x.toml"]),
                    1..4,
                ),
            ) {
                let base = tempfile::TempDir::new().expect("tempdir");
                let root = base.path().join("root");
                fs::create_dir_all(root.join("dir")).expect("mkdir");
                symlink(root.join("dir"), root.join("in_link")).expect("symlink");
                let file = parts.join("/");
                prop_assert!(resolve_contained_path(&root, &file).is_ok(), "{file:?}");
            }
        }
    }
}
