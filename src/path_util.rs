//! Cross-module path normalization helpers.

use std::path::Path;

/// Normalize a root-relative path to forward-slash form.
#[must_use]
pub fn normalize_rel_path(path: &Path) -> String {
    normalize_path_str(&path.to_string_lossy())
}

/// Normalize path separators in a string to `/`.
#[must_use]
pub fn normalize_path_str(path: &str) -> String {
    if path.contains('\\') {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn normalize_path_str_leaves_forward_slashes() {
        assert_eq!(
            normalize_path_str("src/acme/module.py"),
            "src/acme/module.py"
        );
    }

    #[test]
    fn normalize_path_str_replaces_backslashes() {
        assert_eq!(
            normalize_path_str("src\\acme\\module.py"),
            "src/acme/module.py"
        );
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

            #[test]
            fn normalize_path_str_is_idempotent(raw in "\\PC{0,60}") {
                let once = normalize_path_str(&raw);
                prop_assert_eq!(normalize_path_str(&once), once);
            }
        }
    }
}
