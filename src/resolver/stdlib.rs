//! Versioned Python standard library module sets.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::config::TargetVersion;
use crate::manifest::requires_python_max_minor;

static PY310_STDLIB: OnceLock<HashSet<&'static str>> = OnceLock::new();
static PY311_STDLIB: OnceLock<HashSet<&'static str>> = OnceLock::new();
static PY312_STDLIB: OnceLock<HashSet<&'static str>> = OnceLock::new();
static PY313_STDLIB: OnceLock<HashSet<&'static str>> = OnceLock::new();
static PY314_STDLIB: OnceLock<HashSet<&'static str>> = OnceLock::new();

const OLDEST_BUNDLED_MINOR: u32 = 10;
const NEWEST_BUNDLED_MINOR: u32 = 14;

/// Python 3 minors whose stdlib counts: a root that is stdlib in any of them
/// is stdlib, so `tomllib` behind a `sys.version_info` guard is not a missing
/// dependency of a `>=3.10` project (#358).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StdlibRange {
    min: u32,
    max: u32,
}

impl StdlibRange {
    /// From `target` up to the `requires-python` upper bound, or up to the
    /// newest bundled set when nothing caps the minor.
    #[must_use]
    pub fn new(target: &TargetVersion, requires_python: Option<&str>) -> Self {
        let min = target.minor();
        let max = requires_python
            .and_then(requires_python_max_minor)
            .unwrap_or(u32::MAX)
            .max(min);
        Self { min, max }
    }

    /// Returns whether `import_root` is a stdlib module for any minor in range.
    #[must_use]
    pub fn contains(self, import_root: &str) -> bool {
        let low = self.min.clamp(OLDEST_BUNDLED_MINOR, NEWEST_BUNDLED_MINOR);
        let high = self.max.clamp(low, NEWEST_BUNDLED_MINOR);
        (low..=high).any(|minor| stdlib_modules(minor).contains(import_root))
    }
}

fn stdlib_modules(minor: u32) -> &'static HashSet<&'static str> {
    match minor {
        0..=10 => PY310_STDLIB.get_or_init(|| load_modules(include_str!("stdlib/py310.txt"))),
        11 => PY311_STDLIB.get_or_init(|| load_modules(include_str!("stdlib/py311.txt"))),
        12 => PY312_STDLIB.get_or_init(|| load_modules(include_str!("stdlib/py312.txt"))),
        13 => PY313_STDLIB.get_or_init(|| load_modules(include_str!("stdlib/py313.txt"))),
        _ => PY314_STDLIB.get_or_init(|| load_modules(include_str!("stdlib/py314.txt"))),
    }
}

fn load_modules(contents: &'static str) -> HashSet<&'static str> {
    contents.lines().filter(|line| !line.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TargetVersion;

    fn exact(target: &str) -> StdlibRange {
        let target = TargetVersion::parse(target).expect("target");
        let requires = format!("==3.{}.*", target.minor());
        StdlibRange::new(&target, Some(&requires))
    }

    #[test]
    fn recognizes_os_as_stdlib() {
        assert!(exact("py311").contains("os"));
    }

    #[test]
    fn rejects_third_party_root() {
        assert!(!exact("py311").contains("yaml"));
        assert!(!StdlibRange::new(&TargetVersion::default_py311(), None).contains("yaml"));
    }

    #[test]
    fn recognizes_future_as_stdlib() {
        assert!(exact("py310").contains("__future__"));
    }

    #[test]
    fn tomllib_is_stdlib_only_from_py311() {
        assert!(!exact("py310").contains("tomllib"));
        assert!(exact("py311").contains("tomllib"));
    }

    #[test]
    fn platform_and_private_modules_are_stdlib_for_every_target() {
        for version in ["py308", "py310", "py311", "py312", "py313", "py314"] {
            for module in ["unicodedata", "msvcrt", "winreg", "_thread"] {
                assert!(exact(version).contains(module), "{module} on {version}");
            }
        }
    }

    #[test]
    fn distutils_removed_for_py312() {
        assert!(exact("py311").contains("distutils"));
        assert!(!exact("py312").contains("distutils"));
    }

    #[test]
    fn pep594_modules_removed_for_py313() {
        assert!(exact("py312").contains("cgi"));
        assert!(!exact("py313").contains("cgi"));
    }

    #[test]
    fn compression_is_stdlib_only_from_py314() {
        assert!(!exact("py313").contains("compression"));
        assert!(exact("py314").contains("compression"));
        assert!(exact("py314").contains("annotationlib"));
    }

    #[test]
    fn main_module_is_stdlib_for_every_target() {
        for version in ["py310", "py311", "py312", "py313", "py314"] {
            assert!(exact(version).contains("__main__"), "{version}");
        }
    }

    #[test]
    fn range_is_union_of_covered_minors() {
        let py310 = TargetVersion::parse("py310").expect("py310");
        let supported = StdlibRange::new(&py310, Some(">=3.10,<3.15"));
        assert!(supported.contains("tomllib"));
        assert!(supported.contains("cgi"));
        assert!(StdlibRange::new(&py310, None).contains("tomllib"));
        assert!(!StdlibRange::new(&py310, Some(">=3.10,<3.11")).contains("tomllib"));
    }

    #[test]
    fn upper_bound_below_target_keeps_target_set() {
        let py313 = TargetVersion::parse("py313").expect("py313");
        let range = StdlibRange::new(&py313, Some("<3.11"));
        assert!(!range.contains("cgi"));
        assert!(range.contains("tomllib"));
    }
}
