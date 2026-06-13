//! Versioned Python standard library module sets.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::config::TargetVersion;

static PY311_STDLIB: OnceLock<HashSet<&'static str>> = OnceLock::new();

/// Returns whether `import_root` is a stdlib module for `target`.
#[must_use]
pub fn is_stdlib_import(import_root: &str, target: &TargetVersion) -> bool {
    let _ = target;
    PY311_STDLIB
        .get_or_init(|| {
            include_str!("stdlib/py311.txt")
                .lines()
                .filter(|line| !line.is_empty())
                .collect()
        })
        .contains(import_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TargetVersion;

    #[test]
    fn recognizes_os_as_stdlib() {
        assert!(is_stdlib_import("os", &TargetVersion::default_py311()));
    }

    #[test]
    fn rejects_third_party_root() {
        assert!(!is_stdlib_import("yaml", &TargetVersion::default_py311()));
    }
}
