//! Copies from `src/manifest/python_range.rs` (sync-checked) and the two
//! `pep508` types `specifier_range` reads.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Equal,
    EqualStar,
    NotEqual,
    NotEqualStar,
    TildeEqual,
    LessThan,
    LessThanEqual,
    GreaterThan,
    GreaterThanEqual,
    ExactEqual,
}

/// `pep508::VersionSpecifier` without `text`: the only use of `text` in
/// `specifier_range` is a digits-only check, kept as `digits_only`.
pub struct VersionSpecifier {
    pub operator: Operator,
    pub release: Vec<u64>,
    pub digits_only: bool,
}

type Version = [u64; 3];

/// Exclusive bound above every version.
const TOP: Version = [u64::MAX; 3];

/// Release segments above this are not modelled, so a successor or the next
/// minor never overflows into [`TOP`].
const MAX_SEGMENT: u64 = 0xFFFF_FFFF;

/// A set of Python versions: sorted, disjoint, non-empty half-open
/// intervals `[low, high)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonRange(Vec<(Version, Version)>);

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
        Self::from_intervals(chain_copied_collect(&self.0, &other.0)) // stand-in: `.iter().chain(..).copied().collect()`
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

fn specifier_range(specifier: &VersionSpecifier) -> Option<PythonRange> {
    // stand-in: the digits-only check on `specifier.text`
    if !specifier.digits_only {
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

/// Stand-in for `a.iter().chain(b).copied().collect::<Vec<_>>()`: `a`'s
/// elements then `b`'s, in order. Charon is told to keep it opaque; the
/// Lean model is `a ++ b` (`../Chokkin/External.lean`).
pub fn chain_copied_collect(
    a: &[(Version, Version)],
    b: &[(Version, Version)],
) -> Vec<(Version, Version)> {
    let mut out = a.to_vec();
    out.extend_from_slice(b);
    out
}
