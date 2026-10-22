//! Versioned Python standard library module sets.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::config::TargetVersion;
use crate::manifest::{infer_target_version_from_requires_python, requires_python_max_minor};

static STDLIB_MODULES: OnceLock<HashMap<&'static str, Shipped>> = OnceLock::new();

const OLDEST_BUNDLED_MINOR: u32 = 8;
const NEWEST_BUNDLED_MINOR: u32 = 14;

/// Stdlib roots that a given interpreter may still lack: platform-only
/// modules and those built on an optional library or split out by distros.
const NOT_ALWAYS_BUILT: [&str; 52] = [
    "_bz2",
    "_crypt",
    "_ctypes",
    "_curses",
    "_curses_panel",
    "_dbm",
    "_decimal",
    "_gdbm",
    "_hashlib",
    "_lzma",
    "_msi",
    "_overlapped",
    "_posixshmem",
    "_posixsubprocess",
    "_sqlite3",
    "_ssl",
    "_tkinter",
    "_uuid",
    "_winapi",
    "bz2",
    "crypt",
    "ctypes",
    "curses",
    "distutils",
    "ensurepip",
    "fcntl",
    "grp",
    "idlelib",
    "lib2to3",
    "lzma",
    "msilib",
    "msvcrt",
    "nis",
    "nt",
    "ossaudiodev",
    "posix",
    "pty",
    "pwd",
    "pyexpat",
    "readline",
    "resource",
    "spwd",
    "sqlite3",
    "ssl",
    "syslog",
    "termios",
    "tkinter",
    "tty",
    "turtle",
    "venv",
    "winreg",
    "winsound",
];

/// Python 3 minors whose stdlib counts: a root that is stdlib in any of them
/// is stdlib, so `tomllib` behind a `sys.version_info` guard is not a missing
/// dependency of a `>=3.10` project (#358).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StdlibRange {
    min: u32,
    max: u32,
    /// Lowest Python 3 minor `requires-python` allows; `None` when it is
    /// missing or admits Python 2.
    floor: Option<u32>,
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
        let floor = requires_python
            .and_then(infer_target_version_from_requires_python)
            .map(|version| version.minor());
        Self { min, max, floor }
    }

    /// Returns whether `import_root` is a stdlib module for any minor in range.
    #[must_use]
    pub fn contains(self, import_root: &str) -> bool {
        let low = self.min.clamp(OLDEST_BUNDLED_MINOR, NEWEST_BUNDLED_MINOR);
        let high = self.max.clamp(low, NEWEST_BUNDLED_MINOR);
        stdlib_modules()
            .get(import_root)
            .is_some_and(|shipped| shipped.first <= high && low <= shipped.last)
    }

    /// Returns whether only bundled minors outside the range ship
    /// `import_root`, so another range could make it stdlib (#924).
    #[must_use]
    pub fn ships_only_outside(self, import_root: &str) -> bool {
        !self.contains(import_root) && stdlib_modules().contains_key(import_root)
    }

    /// Returns whether every interpreter `requires-python` allows, from its
    /// lower bound up, ships `import_root`, so an `except ImportError:`
    /// fallback for it never runs (#721). `false` without a lower bound.
    #[must_use]
    pub fn always_contains(self, import_root: &str) -> bool {
        let Some(floor) = self.floor.filter(|floor| *floor >= OLDEST_BUNDLED_MINOR) else {
            return false;
        };
        if NOT_ALWAYS_BUILT.contains(&import_root) {
            return false;
        }
        let low = floor.min(NEWEST_BUNDLED_MINOR);
        let high = self.max.clamp(low, NEWEST_BUNDLED_MINOR);
        stdlib_modules()
            .get(import_root)
            .is_some_and(|shipped| shipped.first <= low && high <= shipped.last)
    }
}

/// Bundled minors that ship a stdlib root, as one consecutive run; a
/// `first` of [`OLDEST_BUNDLED_MINOR`] also covers older minors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shipped {
    first: u32,
    last: u32,
}

fn stdlib_modules() -> &'static HashMap<&'static str, Shipped> {
    STDLIB_MODULES.get_or_init(|| parse_modules(include_str!("stdlib/modules.txt")))
}

/// Parses `name [first-last]` lines: a bare name ships in every bundled
/// minor, and an empty `last` means up to [`NEWEST_BUNDLED_MINOR`].
fn parse_modules(contents: &'static str) -> HashMap<&'static str, Shipped> {
    contents
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (name, minors) = line.split_once(' ').unwrap_or((line, "-"));
            let (first, last) = minors.split_once('-')?;
            let minor = |text: &str, default| {
                if text.is_empty() {
                    Some(default)
                } else {
                    text.parse().ok()
                }
            };
            let first = minor(first, OLDEST_BUNDLED_MINOR)?;
            let last = minor(last, NEWEST_BUNDLED_MINOR)?;
            Some((name, Shipped { first, last }))
        })
        .collect()
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
    fn modules_removed_before_py310_are_stdlib_for_older_targets() {
        // `parser` is a builtin module in 3.8 and 3.9, gone in 3.10 (#940).
        for module in ["formatter", "parser", "symbol", "_bootlocale"] {
            assert!(exact("py308").contains(module), "{module}");
            assert!(exact("py309").contains(module), "{module}");
            assert!(!exact("py310").contains(module), "{module}");
        }
        assert!(exact("py308").contains("dummy_threading"));
        assert!(!exact("py309").contains("dummy_threading"));
        assert!(!exact("py308").contains("_peg_parser"));
        assert!(exact("py309").contains("_peg_parser"));
        assert!(!exact("py308").contains("zoneinfo"));
        assert!(exact("py309").contains("zoneinfo"));
    }

    #[test]
    fn modules_only_older_minors_ship_are_version_dependent() {
        let assumed = StdlibRange::new(&TargetVersion::default_py311(), None);
        assert!(assumed.ships_only_outside("formatter"));
        assert!(assumed.ships_only_outside("dummy_threading"));
        assert!(!assumed.ships_only_outside("os"));
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

    fn floor(requires_python: Option<&str>) -> StdlibRange {
        StdlibRange::new(&TargetVersion::default_py311(), requires_python)
    }

    #[test]
    fn always_contains_checks_every_supported_minor_from_the_lower_bound() {
        assert!(floor(Some(">=3.8")).always_contains("threading"));
        assert!(floor(Some(">=3.12")).always_contains("tomllib"));
        // `tomllib` arrived in 3.11 and `cgi` left in 3.13.
        assert!(!floor(Some(">=3.10")).always_contains("tomllib"));
        assert!(!floor(Some(">=3.10")).always_contains("cgi"));
        assert!(floor(Some(">=3.10,<3.13")).always_contains("cgi"));
        assert!(!floor(Some(">=3.8")).always_contains("dummy_threading"));
    }

    #[test]
    fn always_contains_sets_aside_modules_older_floors_lack() {
        // 3.8 lacks `zoneinfo`, and nothing older than 3.8 is bundled.
        assert!(!floor(Some(">=3.8")).always_contains("zoneinfo"));
        assert!(floor(Some(">=3.9")).always_contains("zoneinfo"));
        assert!(!floor(Some(">=3.7")).always_contains("threading"));
    }

    #[test]
    fn always_contains_needs_a_python3_lower_bound() {
        assert!(!floor(None).always_contains("threading"));
        assert!(!floor(Some("<3.13")).always_contains("threading"));
        assert!(!floor(Some(">=2.7,!=3.0.*")).always_contains("threading"));
    }

    #[test]
    fn always_contains_skips_platform_and_optional_builds() {
        for module in ["fcntl", "msvcrt", "sqlite3", "tkinter"] {
            assert!(!floor(Some(">=3.10")).always_contains(module), "{module}");
        }
    }

    #[test]
    fn table_lines_name_the_minors_that_ship_each_root() {
        let table = parse_modules("# comment\n\nos\ntomllib 11-\nasynchat 10-11\n");
        let shipped = |name| (table[name].first, table[name].last);
        assert_eq!(table.len(), 3);
        assert_eq!(shipped("os"), (8, 14));
        assert_eq!(shipped("tomllib"), (11, 14));
        assert_eq!(shipped("asynchat"), (10, 11));
    }

    #[test]
    fn bundled_table_parses_and_matches_the_bundled_minors() {
        let contents = include_str!("stdlib/modules.txt");
        // A bare name or an open range means the generator's oldest or newest
        // version, so a stale constant would misread them.
        let generated = format!("of Python 3.{OLDEST_BUNDLED_MINOR}-3.{NEWEST_BUNDLED_MINOR},");
        assert!(contents.lines().any(|line| line.contains(&generated)));
        let entries = contents
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .count();
        let table = stdlib_modules();
        // A line the parser skipped would silently drop a stdlib root.
        assert_eq!(table.len(), entries);
        for (name, shipped) in table {
            assert!(OLDEST_BUNDLED_MINOR <= shipped.first, "{name}");
            assert!(shipped.first <= shipped.last, "{name}");
            assert!(shipped.last <= NEWEST_BUNDLED_MINOR, "{name}");
        }
    }

    /// One `requires-python` clause on a final `3.minor.patch` release,
    /// written from PEP 440 rather than from `requires_python_max_minor`.
    fn clause_allows(clause: &str, minor: u32, patch: u32) -> bool {
        let (op, version) = clause
            .find(|ch: char| ch.is_ascii_digit())
            .map(|at| clause.split_at(at))
            .expect("clause has a version");
        let star = version.ends_with(".*");
        let parts: Vec<u32> = version
            .trim_end_matches(".*")
            .split('.')
            .map(|part| part.parse().expect("numeric segment"))
            .collect();
        let mut padded = [0; 3];
        padded[..parts.len()].copy_from_slice(&parts);
        let here = [3, minor, patch];
        let prefix = here[..parts.len()] == parts[..];
        match op {
            ">=" => here >= padded,
            ">" => here > padded,
            "<" => here < padded,
            "<=" => here <= padded,
            "==" if star => prefix,
            "!=" if star => !prefix,
            "==" => here == padded,
            "!=" => here != padded,
            // `~=X.Y` is `>=X.Y, ==X.*`; `~=X.Y.Z` is `>=X.Y.Z, ==X.Y.*`.
            "~=" => here >= padded && here[..parts.len() - 1] == parts[..parts.len() - 1],
            _ => panic!("operator {op}"),
        }
    }

    /// Python 3 minors with a patch release every clause allows.
    fn allowed_minors(clauses: &[&str], minors: std::ops::RangeInclusive<u32>) -> Vec<u32> {
        minors
            .filter(|&minor| {
                (0..=4).any(|patch| {
                    clauses
                        .iter()
                        .all(|clause| clause_allows(clause, minor, patch))
                })
            })
            .collect()
    }

    /// The bundled set that stands for `minor`: 3.8's for older minors and
    /// the newest for newer ones.
    fn bundled(minor: u32) -> u32 {
        minor.clamp(OLDEST_BUNDLED_MINOR, NEWEST_BUNDLED_MINOR)
    }

    fn ships(root: &str, minor: u32) -> bool {
        stdlib_modules()
            .get(root)
            .is_some_and(|shipped| (shipped.first..=shipped.last).contains(&bundled(minor)))
    }

    /// §7 step 1 by enumeration (#1109): a root is stdlib when any minor from
    /// `target_version` up to the highest minor `requires-python` allows
    /// ships it. An unbounded specifier reaches every bundled minor, and an
    /// upper bound below the target keeps the target's own set. Exclusions
    /// (`!=3.12.*`) neither split nor narrow the window. `always_contains`
    /// may hold only when every minor `requires-python` allows, from its
    /// lowest, ships the root.
    #[test]
    fn range_follows_the_minors_requires_python_allows() {
        const FAR: u32 = 20;
        let mut clauses: Vec<String> = vec!["<4".to_owned(), ">=2.7".to_owned()];
        for minor in 6..=16 {
            for op in [">=", ">", "<", "<=", "==", "!=", "~="] {
                clauses.push(format!("{op}3.{minor}"));
            }
            for op in ["==", "!="] {
                clauses.push(format!("{op}3.{minor}.*"));
            }
            for op in ["<", "<=", ">", "~="] {
                clauses.push(format!("{op}3.{minor}.1"));
            }
        }
        // Roots that ship in the same minors behave alike: one stands for each
        // run, plus a root built everywhere, one some builds lack, and none.
        let mut runs = std::collections::BTreeMap::new();
        for (name, shipped) in stdlib_modules() {
            let slot = runs.entry((shipped.first, shipped.last)).or_insert(*name);
            *slot = (*slot).min(*name);
        }
        let mut versioned: Vec<&str> = runs.into_values().collect();
        versioned.extend(["fcntl", "yaml"]);
        let specifiers = std::iter::once(Vec::new())
            .chain(clauses.iter().map(|clause| vec![clause.as_str()]))
            .chain(clauses.iter().enumerate().flat_map(|(at, first)| {
                clauses[at + 1..]
                    .iter()
                    .map(move |second| vec![first.as_str(), second.as_str()])
            }));
        let mut checked = 0;
        for specifier in specifiers {
            let allowed = allowed_minors(&specifier, 0..=FAR);
            if !specifier.is_empty() && allowed.is_empty() {
                // No Python 3 can run the project; nothing to compare.
                continue;
            }
            let joined = specifier.join(",");
            let requires = (!specifier.is_empty()).then_some(joined.as_str());
            let bounds: Vec<&str> = specifier
                .iter()
                .copied()
                .filter(|clause| !clause.starts_with("!="))
                .collect();
            let upper = allowed_minors(&bounds, 0..=FAR)
                .last()
                .copied()
                .filter(|&minor| minor < FAR);
            for target in 7..=15 {
                let target_version =
                    TargetVersion::parse(&format!("py3{target:02}")).expect("target");
                let range = StdlibRange::new(&target_version, requires);
                let top = upper.map_or(NEWEST_BUNDLED_MINOR, |upper| upper.max(target));
                let window: Vec<u32> = (target..=top.max(target)).collect();
                for root in &versioned {
                    let stdlib = window.iter().any(|&minor| ships(root, minor));
                    let case = format!("{root} for py3{target:02} with {joined:?}");
                    assert_eq!(range.contains(root), stdlib, "contains: {case}");
                    let elsewhere = (OLDEST_BUNDLED_MINOR..=NEWEST_BUNDLED_MINOR)
                        .any(|minor| ships(root, minor));
                    assert_eq!(
                        range.ships_only_outside(root),
                        !stdlib && elsewhere,
                        "ships_only_outside: {case}"
                    );
                    let always = requires.is_some()
                        && allowed
                            .first()
                            .is_some_and(|&low| low >= OLDEST_BUNDLED_MINOR)
                        && !NOT_ALWAYS_BUILT.contains(root)
                        && allowed.iter().all(|&minor| ships(root, minor));
                    if range.always_contains(root) {
                        assert!(always, "always_contains: {case}");
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 100_000, "{checked}");
    }

    #[test]
    fn upper_bound_below_target_keeps_target_set() {
        let py313 = TargetVersion::parse("py313").expect("py313");
        let range = StdlibRange::new(&py313, Some("<3.11"));
        assert!(!range.contains("cgi"));
        assert!(range.contains("tomllib"));
    }
}
