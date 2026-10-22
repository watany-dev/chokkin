//! The Python versions `requires-python` allows and a `python_version` /
//! `python_full_version` marker holds on (#919), so CHK002 can tell a
//! marker every supported interpreter satisfies from one that may leave the
//! dependency uninstalled.
//!
//! Versions are final `X.Y.Z` releases: an interpreter's
//! `python_full_version` is one, and pip checks `requires-python` against
//! it. Input outside that model (`===`, a pre-, post- or dev-release, a
//! local version, an epoch, more than 3 release segments, another marker
//! variable, `in`) reads as `None`, so the caller keeps the dependency
//! conditional: a marker not read is never taken as always true.

use super::pep508::{Operator, VersionSpecifier, parse_version_specifiers, strip_python};
use super::types::ProjectMetadata;

type Version = [u64; 3];

/// Exclusive bound above every version.
const TOP: Version = [u64::MAX; 3];

/// Release segments above this are not modelled, so a successor or the next
/// minor never overflows into [`TOP`].
const MAX_SEGMENT: u64 = 0xFFFF_FFFF;

/// Nesting cap so adversarial `((((…` input cannot exhaust the stack.
const MAX_DEPTH: usize = 64;

/// A set of Python versions: sorted, disjoint, non-empty half-open
/// intervals `[low, high)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PythonRange(Vec<(Version, Version)>);

impl PythonRange {
    fn all() -> Self {
        Self(vec![([0; 3], TOP)])
    }

    fn from_intervals(mut intervals: Vec<(Version, Version)>) -> Self {
        intervals.retain(|(low, high)| low < high);
        intervals.sort_unstable();
        let mut merged: Vec<(Version, Version)> = Vec::with_capacity(intervals.len());
        for (low, high) in intervals {
            match merged.last_mut() {
                Some(last) if low <= last.1 => last.1 = last.1.max(high),
                _ => merged.push((low, high)),
            }
        }
        Self(merged)
    }

    fn union(&self, other: &Self) -> Self {
        Self::from_intervals(self.0.iter().chain(&other.0).copied().collect())
    }

    fn complement(&self) -> Self {
        let mut gaps = Vec::new();
        let mut start = [0; 3];
        for &(low, high) in &self.0 {
            gaps.push((start, low));
            start = high;
        }
        gaps.push((start, TOP));
        Self::from_intervals(gaps)
    }

    fn intersect(&self, other: &Self) -> Self {
        self.complement().union(&other.complement()).complement()
    }

    fn is_subset_of(&self, other: &Self) -> bool {
        self.intersect(&other.complement()).0.is_empty()
    }
}

/// The interpreters a project supports: `requires-python` and Poetry's
/// `python` dependency, each read on its own, so one that is not read still
/// leaves the other's bound. `None` when neither bounds them.
pub(crate) fn supported_python(metadata: &ProjectMetadata) -> Option<PythonRange> {
    let ranges = [
        metadata
            .requires_python
            .as_deref()
            .and_then(requires_python_range),
        metadata
            .poetry_python_marker
            .as_deref()
            .and_then(marker_range),
    ];
    ranges.into_iter().flatten().reduce(|a, b| a.intersect(&b))
}

pub(crate) fn marker_always_holds(marker: &str, supported: &PythonRange) -> bool {
    marker_range(marker).is_some_and(|range| supported.is_subset_of(&range))
}

pub(crate) fn requires_python_range(specifier: &str) -> Option<PythonRange> {
    parse_version_specifiers(specifier)?
        .iter()
        .map(specifier_range)
        .try_fold(PythonRange::all(), |range, next| {
            Some(range.intersect(&next?))
        })
}

fn specifier_range(specifier: &VersionSpecifier) -> Option<PythonRange> {
    let version = specifier.text.trim_start_matches(['<', '>', '=', '!', '~']);
    let digits = version.strip_suffix(".*").unwrap_or(version);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let release = &specifier.release;
    if release.iter().any(|&segment| segment > MAX_SEGMENT) {
        return None;
    }
    let point = || exact(release);
    let range = |low, high| Some(PythonRange::from_intervals(vec![(low, high)]));
    match specifier.operator {
        Operator::GreaterThanEqual => range(point()?, TOP),
        Operator::GreaterThan => range(successor(point()?), TOP),
        Operator::LessThan => range([0; 3], point()?),
        Operator::LessThanEqual => range([0; 3], successor(point()?)),
        Operator::Equal => range(point()?, successor(point()?)),
        Operator::NotEqual => Some(range(point()?, successor(point()?))?.complement()),
        Operator::EqualStar => prefix_range(release),
        Operator::NotEqualStar => Some(prefix_range(release)?.complement()),
        // `~=3.8.1` is `>=3.8.1, ==3.8.*`.
        Operator::TildeEqual if release.len() <= 3 => {
            let at_least = range(pad(release), TOP)?;
            Some(at_least.intersect(&prefix_range(&release[..release.len() - 1])?))
        },
        Operator::TildeEqual | Operator::ExactEqual => None,
    }
}

/// `release` as a 3-segment version; `None` past 3 segments unless the
/// rest are zero (`3.8.0.0` is `3.8.0`).
fn exact(release: &[u64]) -> Option<Version> {
    if release.iter().skip(3).any(|&segment| segment != 0) {
        return None;
    }
    Some(pad(release))
}

fn pad(release: &[u64]) -> Version {
    let mut version = [0; 3];
    for (slot, segment) in version.iter_mut().zip(release) {
        *slot = *segment;
    }
    version
}

fn successor([major, minor, patch]: Version) -> Version {
    [major, minor, patch + 1]
}

/// The versions `==<prefix>.*` matches.
fn prefix_range(prefix: &[u64]) -> Option<PythonRange> {
    if prefix.is_empty() || prefix.len() > 3 {
        return None;
    }
    let low = pad(prefix);
    let mut high = low;
    high[prefix.len() - 1] += 1;
    Some(PythonRange::from_intervals(vec![(low, high)]))
}

/// The versions on which a marker made only of `python_version` /
/// `python_full_version` comparisons holds.
pub(crate) fn marker_range(marker: &str) -> Option<PythonRange> {
    let tokens = tokenize(marker)?;
    let mut pos = 0;
    let range = parse_or(&tokens, &mut pos, 0)?;
    (pos == tokens.len()).then_some(range)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Open,
    Close,
    Quoted(&'a str),
    Op(&'a str),
    Word(&'a str),
}

fn tokenize(input: &str) -> Option<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let mut rest = strip_python(input);
    while let Some(ch) = rest.chars().next() {
        let len = match ch {
            '(' => {
                tokens.push(Token::Open);
                1
            },
            ')' => {
                tokens.push(Token::Close);
                1
            },
            '\'' | '"' => {
                let close = rest[1..].find(ch)?;
                tokens.push(Token::Quoted(&rest[1..=close]));
                close + 2
            },
            '<' | '>' | '=' | '!' | '~' => {
                let len = rest
                    .find(|c: char| !matches!(c, '<' | '>' | '=' | '!' | '~'))
                    .unwrap_or(rest.len());
                tokens.push(Token::Op(&rest[..len]));
                len
            },
            _ => {
                let len = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.')))
                    .unwrap_or(rest.len());
                tokens.push(Token::Word(&rest[..len]));
                len
            },
        };
        // A token that consumes nothing (a character no arm reads) would
        // loop forever; the input is outside the model instead.
        if len == 0 {
            return None;
        }
        rest = rest[len..].trim_start_matches([' ', '\t']);
    }
    Some(tokens)
}

fn parse_or(tokens: &[Token<'_>], pos: &mut usize, depth: usize) -> Option<PythonRange> {
    let mut range = parse_and(tokens, pos, depth)?;
    while tokens.get(*pos) == Some(&Token::Word("or")) {
        *pos += 1;
        range = range.union(&parse_and(tokens, pos, depth)?);
    }
    Some(range)
}

fn parse_and(tokens: &[Token<'_>], pos: &mut usize, depth: usize) -> Option<PythonRange> {
    let mut range = parse_expr(tokens, pos, depth)?;
    while tokens.get(*pos) == Some(&Token::Word("and")) {
        *pos += 1;
        range = range.intersect(&parse_expr(tokens, pos, depth)?);
    }
    Some(range)
}

fn parse_expr(tokens: &[Token<'_>], pos: &mut usize, depth: usize) -> Option<PythonRange> {
    if tokens.get(*pos) == Some(&Token::Open) {
        if depth >= MAX_DEPTH {
            return None;
        }
        *pos += 1;
        let range = parse_or(tokens, pos, depth + 1)?;
        if tokens.get(*pos) != Some(&Token::Close) {
            return None;
        }
        *pos += 1;
        return Some(range);
    }
    let comparison = tokens.get(*pos..*pos + 3)?;
    *pos += 3;
    let (variable, op, value) = match *comparison {
        [Token::Word(variable), Token::Op(op), Token::Quoted(value)] => (variable, op, value),
        // `packaging` builds the specifier from the environment's value
        // (`'3.8' <= python_version` is `Specifier("<=3.9").contains("3.8")`),
        // which matches the flipped comparison only for the ordering operators.
        [Token::Quoted(value), Token::Op(op), Token::Word(variable)] => {
            let flipped = match op {
                "<" => ">",
                "<=" => ">=",
                ">" => "<",
                ">=" => "<=",
                _ => return None,
            };
            (variable, flipped, value)
        },
        _ => return None,
    };
    // A marker compares against one specifier; `packaging` rejects a
    // comma, which `parse_version_specifiers` reads as a set.
    if value.contains(',') {
        return None;
    }
    let specifier = parse_version_specifiers(&format!("{op}{value}"))?;
    let [specifier] = specifier.as_slice() else {
        return None;
    };
    let range = specifier_range(specifier)?;
    match variable {
        "python_full_version" => Some(range),
        "python_version" => Some(minor_blocks(&range)),
        _ => None,
    }
}

/// The full versions whose `python_version` (`X.Y`, compared as `X.Y.0`)
/// is in `range`: `X.Y.0` is in `[low, high)` exactly when the full version
/// is in `[up(low), up(high))`, `up` rounding up to a minor boundary.
fn minor_blocks(range: &PythonRange) -> PythonRange {
    let up = |version: Version| match version {
        [_, _, 0] => version,
        _ if version == TOP => TOP,
        [major, minor, _] => [major, minor + 1, 0],
    };
    PythonRange::from_intervals(
        range
            .0
            .iter()
            .map(|&(low, high)| (up(low), up(high)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn implied(requires: &str, marker: &str) -> bool {
        let supported = requires_python_range(requires).expect("requires-python");
        marker_always_holds(marker, &supported)
    }

    #[test]
    fn lower_bound_markers_at_or_below_requires_python_are_implied() {
        assert!(implied(">=3.9", "python_version >= '3.8'"));
        assert!(implied(">=3.9", "python_version >= \"3.9\""));
        assert!(implied(">=3.9", "python_full_version >= '3.9.0'"));
        assert!(implied(">=3.9", "python_version > '3.8'"));
        assert!(implied(">=3.9", "'3.8' <= python_version"));
        assert!(implied(">=3.9,<3.13", "python_version < '3.13'"));
        assert!(implied(
            "~=3.10",
            "python_version >= '3.10' and python_version < '4'"
        ));
        assert!(implied("==3.11.*", "python_version == '3.11'"));
        assert!(implied(">=3.9", "(python_version >= '3.6')"));
        assert!(implied(
            ">=3.9",
            "python_version >= '2.7' and python_version != '3.8'"
        ));
        assert!(implied(
            ">=3.9",
            "python_version < '3.0' or python_version >= '3.6'"
        ));
    }

    #[test]
    fn markers_a_supported_interpreter_fails_are_not_implied() {
        assert!(!implied(">=3.9", "python_version < '3.12'"));
        assert!(!implied(">=3.9", "python_version >= '3.10'"));
        // 3.9.0 is allowed but its `python_version` 3.9 is not above 3.9.
        assert!(!implied(">=3.9", "python_version > '3.9'"));
        assert!(!implied(">=3.9.1", "python_full_version >= '3.9.2'"));
        // `python_version` of 3.9.x is 3.9, below 3.9.1.
        assert!(!implied(">=3.9", "python_version >= '3.9.1'"));
        assert!(!implied(">=3.9", "python_version != '3.10'"));
    }

    #[test]
    fn markers_outside_the_model_are_never_implied() {
        for marker in [
            "python_version >= '3.8' and sys_platform == 'linux'",
            "sys_platform == 'linux'",
            // Always true, but `sys_platform` is not modelled.
            "python_version >= '3.8' or sys_platform == 'win32'",
            "python_version in '3.9 3.10 3.11'",
            "python_version === '3.9'",
            // One specifier, not a set: `packaging` rejects the comma.
            "python_version >= '3.8,'",
            "python_version >= '3.8,<4'",
            "python_version >= '3.8rc1'",
            "python_full_version >= '3.8.0.1'",
            "'3.8' == python_version",
            "python ^3.8",
            "",
        ] {
            assert!(!implied(">=3.9", marker), "{marker}");
        }
        assert_eq!(requires_python_range(">=3.9rc1"), None);
        assert_eq!(requires_python_range("===3.9"), None);
        assert_eq!(requires_python_range("not a specifier"), None);
    }

    #[test]
    fn python_version_rounds_bounds_up_to_minor_blocks() {
        // `python_version > '3.8.1'` holds from 3.9 on.
        assert_eq!(
            marker_range("python_version > '3.8.1'"),
            Some(PythonRange(vec![([3, 9, 0], TOP)]))
        );
        assert_eq!(
            marker_range("python_version == '3.8'"),
            Some(PythonRange(vec![([3, 8, 0], [3, 9, 0])]))
        );
        assert_eq!(
            marker_range("python_version == '3.8.1'"),
            Some(PythonRange(Vec::new()))
        );
    }

    #[test]
    fn requires_python_and_poetry_python_both_bound_the_interpreters() {
        let metadata = ProjectMetadata {
            requires_python: Some(">=3.8".to_owned()),
            poetry_python_marker: Some(
                "python_version >= \"3.9\" and python_version < \"4\"".to_owned(),
            ),
            ..ProjectMetadata::default()
        };
        let supported = supported_python(&metadata).expect("bounded");
        assert!(marker_always_holds("python_version >= '3.9'", &supported));
        assert!(!marker_always_holds("python_version >= '3.10'", &supported));
        // An unread bound leaves the other one.
        let metadata = ProjectMetadata {
            requires_python: Some("nonsense".to_owned()),
            poetry_python_marker: Some("python_version >= \"3.9\"".to_owned()),
            ..ProjectMetadata::default()
        };
        assert!(supported_python(&metadata).is_some());
        assert_eq!(supported_python(&ProjectMetadata::default()), None);
    }

    #[test]
    fn set_operations_merge_and_split_intervals() {
        let range =
            |intervals: &[(Version, Version)]| PythonRange::from_intervals(intervals.to_vec());
        let a = range(&[([3, 8, 0], [3, 10, 0]), ([3, 10, 0], [3, 12, 0])]);
        assert_eq!(a, range(&[([3, 8, 0], [3, 12, 0])]));
        let b = range(&[([3, 9, 0], [3, 9, 1])]);
        assert_eq!(
            a.intersect(&b.complement()),
            range(&[([3, 8, 0], [3, 9, 0]), ([3, 9, 1], [3, 12, 0])])
        );
        assert!(b.is_subset_of(&a));
        assert!(!a.is_subset_of(&b));
        assert!(PythonRange::all().complement().0.is_empty());
    }

    #[test]
    fn a_version_on_the_left_flips_the_ordering_operator() {
        for (flipped, direct) in [
            ("'3.8' < python_version", "python_version > '3.8'"),
            ("'3.8' <= python_version", "python_version >= '3.8'"),
            ("'3.8' > python_version", "python_version < '3.8'"),
            ("'3.8' >= python_version", "python_version <= '3.8'"),
        ] {
            let range = marker_range(flipped);
            assert!(range.is_some(), "{flipped}");
            assert_eq!(range, marker_range(direct), "{flipped}");
        }
        assert_eq!(marker_range("'3.8' == python_version"), None);
    }

    #[test]
    fn nesting_past_the_depth_limit_is_outside_the_model() {
        let nested = |depth| {
            format!(
                "{}python_version >= '3.8'{}",
                "(".repeat(depth),
                ")".repeat(depth)
            )
        };
        assert!(marker_range(&nested(MAX_DEPTH)).is_some());
        assert_eq!(marker_range(&nested(MAX_DEPTH + 1)), None);
    }

    #[test]
    fn requires_python_specifiers_map_to_exact_intervals() {
        let interval = |low, high| Some(PythonRange(vec![(low, high)]));
        assert_eq!(
            requires_python_range("~=3.10"),
            interval([3, 10, 0], [4, 0, 0])
        );
        assert_eq!(
            requires_python_range("~=3.8.1"),
            interval([3, 8, 1], [3, 9, 0])
        );
        assert_eq!(requires_python_range("~=3.8.1.2"), None);
        assert_eq!(
            requires_python_range("==3.11.*"),
            interval([3, 11, 0], [3, 12, 0])
        );
        assert_eq!(
            requires_python_range("==3.8.1.*"),
            interval([3, 8, 1], [3, 8, 2])
        );
        assert_eq!(requires_python_range("==3.8.1.2.*"), None);
        let max = [3, MAX_SEGMENT, 0];
        assert_eq!(
            requires_python_range(&format!(">=3.{MAX_SEGMENT}")),
            interval(max, TOP)
        );
        assert_eq!(
            requires_python_range(&format!(">=3.{}", MAX_SEGMENT + 1)),
            None
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Generated release segments stay at or below this, so the grid's
        /// last value per segment lies above every bound they write.
        const SEGMENT: u64 = 3;

        /// The final releases the reference models are evaluated on.
        fn grid() -> impl Iterator<Item = Version> {
            let values = || 0..=SEGMENT + 1;
            values().flat_map(move |major| {
                values().flat_map(move |minor| values().map(move |patch| [major, minor, patch]))
            })
        }

        fn contains(range: &PythonRange, version: Version) -> bool {
            range
                .0
                .iter()
                .any(|&(low, high)| low <= version && version < high)
        }

        /// Sorted, non-empty, and neither overlapping nor touching, so two
        /// ranges are the same set exactly when they are equal.
        fn is_canonical(range: &PythonRange) -> bool {
            range.0.iter().all(|(low, high)| low < high)
                && range.0.windows(2).all(|pair| pair[0].1 < pair[1].0)
        }

        fn endpoint() -> impl Strategy<Value = Version> {
            let segment = || 0..=SEGMENT + 1;
            prop_oneof![
                4 => [segment(), segment(), segment()],
                1 => Just(TOP),
            ]
        }

        fn range() -> impl Strategy<Value = PythonRange> {
            prop::collection::vec((endpoint(), endpoint()), 0..4)
                .prop_map(PythonRange::from_intervals)
        }

        fn release() -> impl Strategy<Value = Vec<u64>> {
            prop::collection::vec(0..=SEGMENT, 1..=3)
        }

        /// One specifier the model reads: the operator, its release, and
        /// whether it is a `.*` prefix match.
        #[derive(Debug, Clone)]
        struct Spec {
            op: &'static str,
            release: Vec<u64>,
            star: bool,
        }

        impl Spec {
            fn render(&self) -> String {
                let version = self
                    .release
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(".");
                let star = if self.star { ".*" } else { "" };
                format!("{}{version}{star}", self.op)
            }

            /// PEP 440 on a final release, written from the spec rather than
            /// from the intervals: zero padding, prefix matching, and `~=V`
            /// as `>=V` with all but the last segment fixed.
            fn holds(&self, version: Version) -> bool {
                let value = pad(&self.release);
                let prefix = |len: usize| version[..len] == value[..len];
                match (self.op, self.star) {
                    ("==", true) => prefix(self.release.len()),
                    ("!=", true) => !prefix(self.release.len()),
                    ("==", false) => version == value,
                    ("!=", false) => version != value,
                    ("<", _) => version < value,
                    ("<=", _) => version <= value,
                    (">", _) => version > value,
                    (">=", _) => version >= value,
                    // `~=`, the one operator left.
                    _ => version >= value && prefix(self.release.len() - 1),
                }
            }
        }

        fn spec() -> impl Strategy<Value = Spec> {
            (
                prop::sample::select(["==", "!=", "<", "<=", ">", ">=", "~="].as_slice()),
                release(),
                any::<bool>(),
            )
                .prop_map(|(op, mut release, star)| {
                    if op == "~=" && release.len() < 2 {
                        release.push(0);
                    }
                    Spec {
                        op,
                        release,
                        star: star && matches!(op, "==" | "!="),
                    }
                })
        }

        /// A comparison marker for an interpreter `X.Y.Z`.
        #[derive(Debug, Clone)]
        enum Marker {
            Compare {
                full: bool,
                spec: Spec,
                /// Written `'value' op variable`, with the operator mirrored.
                flipped: bool,
            },
            /// A comparison outside the model.
            Unread(&'static str),
            And(Box<Self>, Box<Self>),
            Or(Box<Self>, Box<Self>),
            Group(Box<Self>),
        }

        impl Marker {
            fn render(&self) -> String {
                match self {
                    Self::Compare {
                        full,
                        spec,
                        flipped,
                    } => {
                        let variable = if *full {
                            "python_full_version"
                        } else {
                            "python_version"
                        };
                        let text = spec.render();
                        let value = &text[spec.op.len()..];
                        if *flipped {
                            let mirrored = match spec.op {
                                "<" => ">",
                                "<=" => ">=",
                                ">" => "<",
                                // `>=`: only the ordering operators are flipped.
                                _ => "<=",
                            };
                            format!("'{value}' {mirrored} {variable}")
                        } else {
                            format!("{variable} {} \"{value}\"", spec.op)
                        }
                    },
                    Self::Unread(text) => (*text).to_owned(),
                    // `and` binds tighter, so an `or` beneath it needs parentheses.
                    Self::And(left, right) => {
                        let operand = |side: &Self| match side {
                            Self::Or(..) => format!("({})", side.render()),
                            _ => side.render(),
                        };
                        format!("{} and {}", operand(left), operand(right))
                    },
                    Self::Or(left, right) => format!("{} or {}", left.render(), right.render()),
                    Self::Group(inner) => format!("({})", inner.render()),
                }
            }

            /// Whether the marker holds on `version`; `None` when it reads
            /// a comparison outside the model.
            fn holds(&self, version: Version) -> Option<bool> {
                match self {
                    Self::Compare { full, spec, .. } => {
                        let [major, minor, _] = version;
                        let value = if *full { version } else { [major, minor, 0] };
                        Some(spec.holds(value))
                    },
                    Self::Unread(_) => None,
                    Self::And(left, right) => Some(left.holds(version)? & right.holds(version)?),
                    Self::Or(left, right) => Some(left.holds(version)? | right.holds(version)?),
                    Self::Group(inner) => inner.holds(version),
                }
            }

            fn is_read(&self) -> bool {
                self.holds([0; 3]).is_some()
            }
        }

        /// Comparisons outside the model (module docs): each one alone,
        /// and any marker holding one, reads as `None`.
        const UNREAD: [&str; 14] = [
            "python_version === '3.8'",
            "python_version >= '3.8rc1'",
            "python_full_version >= '3.8.0.post1'",
            "python_version < '3.8.dev0'",
            "python_full_version == '3.8.0+local'",
            "python_version >= '1!3.8'",
            "python_full_version >= '3.8.0.1'",
            "python_version == '3.8.0.1.*'",
            "python_version ~= '3.8.0.1'",
            "sys_platform == 'linux'",
            "implementation_name == 'cpython'",
            "platform_python_implementation != 'PyPy'",
            "python_version in '3.8 3.9'",
            "python_version not in '3.8'",
        ];

        fn compare() -> impl Strategy<Value = Marker> {
            (any::<bool>(), spec(), any::<bool>()).prop_map(|(full, spec, flipped)| {
                let flipped = flipped && matches!(spec.op, "<" | "<=" | ">" | ">=");
                Marker::Compare {
                    full,
                    spec,
                    flipped,
                }
            })
        }

        fn marker(unread: bool) -> impl Strategy<Value = Marker> {
            let leaf = if unread {
                prop_oneof![
                    3 => compare(),
                    1 => prop::sample::select(UNREAD.as_slice()).prop_map(Marker::Unread),
                ]
                .boxed()
            } else {
                compare().boxed()
            };
            leaf.prop_recursive(3, 8, 2, |inner| {
                prop_oneof![
                    (inner.clone(), inner.clone())
                        .prop_map(|(left, right)| Marker::And(Box::new(left), Box::new(right))),
                    (inner.clone(), inner.clone())
                        .prop_map(|(left, right)| Marker::Or(Box::new(left), Box::new(right))),
                    inner.prop_map(|inner| Marker::Group(Box::new(inner))),
                ]
            })
        }

        proptest! {
            #[test]
            fn set_operations_satisfy_the_boolean_algebra_laws(
                a in range(),
                b in range(),
                c in range(),
            ) {
                for result in [a.union(&b), a.intersect(&b), a.complement()] {
                    prop_assert!(is_canonical(&result), "{result:?}");
                }
                prop_assert_eq!(a.union(&b), b.union(&a));
                prop_assert_eq!(a.intersect(&b), b.intersect(&a));
                prop_assert_eq!(a.union(&b).union(&c), a.union(&b.union(&c)));
                prop_assert_eq!(a.intersect(&b).intersect(&c), a.intersect(&b.intersect(&c)));
                prop_assert_eq!(&a.union(&a.intersect(&b)), &a);
                prop_assert_eq!(&a.intersect(&a.union(&b)), &a);
                prop_assert_eq!(a.union(&b).complement(), a.complement().intersect(&b.complement()));
                prop_assert_eq!(a.intersect(&b).complement(), a.complement().union(&b.complement()));
                prop_assert_eq!(&a.complement().complement(), &a);
                prop_assert_eq!(a.is_subset_of(&b), a.intersect(&b) == a);
                prop_assert!(a.intersect(&b).is_subset_of(&a));
                prop_assert!(a.is_subset_of(&a.union(&b)));
            }

            #[test]
            fn set_operations_match_membership(a in range(), b in range()) {
                let (union, intersection, complement) =
                    (a.union(&b), a.intersect(&b), a.complement());
                for version in grid() {
                    let (in_a, in_b) = (contains(&a, version), contains(&b, version));
                    prop_assert_eq!(contains(&union, version), in_a || in_b);
                    prop_assert_eq!(contains(&intersection, version), in_a && in_b);
                    prop_assert_eq!(contains(&complement, version), !in_a);
                }
            }

            #[test]
            fn marker_range_matches_evaluating_the_marker(marker in marker(false)) {
                let text = marker.render();
                let range = marker_range(&text);
                prop_assert!(range.is_some(), "{text}");
                let range = range.unwrap_or_else(PythonRange::all);
                for version in grid() {
                    prop_assert_eq!(
                        Some(contains(&range, version)),
                        marker.holds(version),
                        "{} on {:?}", text, version
                    );
                }
            }

            #[test]
            fn requires_python_range_matches_evaluating_the_specifiers(
                specs in prop::collection::vec(spec(), 1..4),
            ) {
                let text = specs.iter().map(Spec::render).collect::<Vec<_>>().join(", ");
                let range = requires_python_range(&text);
                prop_assert!(range.is_some(), "{text}");
                let range = range.unwrap_or_else(PythonRange::all);
                for version in grid() {
                    prop_assert_eq!(
                        contains(&range, version),
                        specs.iter().all(|spec| spec.holds(version)),
                        "{} on {:?}", text, version
                    );
                }
            }

            /// A marker holding any comparison outside the model is not read,
            /// so it is never taken as always true, whatever the project
            /// supports.
            #[test]
            fn markers_outside_the_model_are_never_read(
                marker in marker(true).prop_filter("reads a comparison outside the model", |marker| {
                    !marker.is_read()
                }),
                supported in prop_oneof![
                    range(),
                    prop::collection::vec(spec(), 1..4).prop_filter_map("read", |specs| {
                        let text = specs.iter().map(Spec::render).collect::<Vec<_>>().join(",");
                        requires_python_range(&text)
                    }),
                ],
            ) {
                let text = marker.render();
                prop_assert_eq!(marker_range(&text), None, "{}", text);
                prop_assert!(!marker_always_holds(&text, &supported), "{}", text);
                prop_assert!(!marker_always_holds(&text, &PythonRange(Vec::new())), "{}", text);
            }

            #[test]
            fn specifiers_outside_the_model_are_never_read(
                specs in prop::collection::vec(spec(), 0..3),
                unread in prop::sample::select(
                    ["===3.9", ">=3.9rc1", ">=3.9.post1", "<3.9.dev0", "==3.9+local",
                     ">=1!3.9", ">=3.9.0.1", "==3.9.0.1.*", "~=3.9.0.1", "~=3"].as_slice(),
                ),
                at in any::<prop::sample::Index>(),
            ) {
                let mut items: Vec<String> = specs.iter().map(Spec::render).collect();
                items.insert(at.index(items.len() + 1), unread.to_owned());
                let text = items.join(", ");
                prop_assert_eq!(requires_python_range(&text), None, "{}", text);
            }

            /// Any input, nested past the depth limit or not, reads as a range
            /// or `None` without panicking.
            #[test]
            fn arbitrary_input_never_panics(
                open in 0usize..2 * MAX_DEPTH,
                close in 0usize..2 * MAX_DEPTH,
                body in r#"[()'" <>=!~,.*a-z_0-9]{0,40}"#,
            ) {
                let text = format!("{}{body}{}", "(".repeat(open), ")".repeat(close));
                let _ = marker_range(&text);
                let _ = requires_python_range(&body);
                let _ = marker_always_holds(&text, &PythonRange::all());
            }
        }
    }
}
