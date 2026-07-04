//! Cross-module path normalization helpers.

use std::path::Path;

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
