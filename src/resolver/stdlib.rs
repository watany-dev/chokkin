//! Versioned Python standard library module sets.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::config::TargetVersion;

static PY310_STDLIB: OnceLock<HashSet<&'static str>> = OnceLock::new();
static PY311_STDLIB: OnceLock<HashSet<&'static str>> = OnceLock::new();
static PY312_STDLIB: OnceLock<HashSet<&'static str>> = OnceLock::new();
static PY313_STDLIB: OnceLock<HashSet<&'static str>> = OnceLock::new();

/// Returns whether `import_root` is a stdlib module for `target`.
#[must_use]
pub fn is_stdlib_import(import_root: &str, target: &TargetVersion) -> bool {
    stdlib_modules(target).contains(import_root)
}

fn stdlib_modules(target: &TargetVersion) -> &'static HashSet<&'static str> {
    match target_minor(target) {
        0..=10 => PY310_STDLIB.get_or_init(|| load_modules(include_str!("stdlib/py310.txt"))),
        11 => PY311_STDLIB.get_or_init(|| load_modules(include_str!("stdlib/py311.txt"))),
        12 => PY312_STDLIB.get_or_init(|| load_modules(include_str!("stdlib/py312.txt"))),
        _ => PY313_STDLIB.get_or_init(|| load_modules(include_str!("stdlib/py313.txt"))),
    }
}

fn load_modules(contents: &'static str) -> HashSet<&'static str> {
    contents.lines().filter(|line| !line.is_empty()).collect()
}

fn target_minor(target: &TargetVersion) -> u32 {
    let value = target.as_str();
    let suffix = value.strip_prefix("py3").unwrap_or("11");
    suffix.parse().unwrap_or(11)
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

    #[test]
    fn recognizes_future_as_stdlib() {
        assert!(is_stdlib_import(
            "__future__",
            &TargetVersion::parse("py310").expect("py310")
        ));
    }

    #[test]
    fn tomllib_is_stdlib_only_from_py311() {
        let py310 = TargetVersion::parse("py310").expect("py310");
        let py311 = TargetVersion::default_py311();
        assert!(!is_stdlib_import("tomllib", &py310));
        assert!(is_stdlib_import("tomllib", &py311));
    }

    #[test]
    fn pep594_modules_removed_for_py313() {
        let py312 = TargetVersion::parse("py312").expect("py312");
        let py313 = TargetVersion::parse("py313").expect("py313");
        assert!(is_stdlib_import("cgi", &py312));
        assert!(!is_stdlib_import("cgi", &py313));
    }
}
