//! `sys.version_info` comparisons in branch tests: the Python 3 minors a
//! branch can run on (#997).
//!
//! Only `sys.version_info` and `sys.version_info[:2]` compared once with a
//! tuple of one or two integer literals are read, from either side, with
//! `<` / `<=` / `>` / `>=` (and `==` / `!=` for `[:2]`). Any other test is an
//! opaque leaf, so a branch it guards may run on every minor (ADR 0005).

use ruff_python_ast::{CmpOp, Expr, ExprNumberLiteral, Number};

use super::condition::{self, Leaf};
use super::platform_guard::{PlatformModule, PlatformModules, is_sys_attribute};

/// Python 3 minors as bits: bit `n` is 3.`n`, and the last bit also stands
/// for every later minor, since no tuple read names one past it.
pub(super) type Minors = u128;

/// Every minor: no version test bounds the branch.
pub(super) const ALL_MINORS: Minors = Minors::MAX;

/// The last minor with a bit of its own.
const LAST_MINOR: u32 = Minors::BITS - 1;

/// One read comparison: `version_info <op> (major, minor)`, with the
/// version on the left.
struct VersionTest {
    op: CmpOp,
    major: u32,
    minor: Option<u32>,
    /// `sys.version_info` itself, which is longer than any tuple it is
    /// compared with: `(3, 9, 0, "final", 0) > (3, 9)`.
    full: bool,
}

impl VersionTest {
    fn read(expr: &Expr, modules: &PlatformModules) -> Option<Self> {
        let Expr::Compare(compare) = expr else {
            return None;
        };
        let ([op], [left, right]) = (&*compare.ops, &*compare.operands) else {
            return None;
        };
        let (full, tuple, op) = if let Some(full) = version_value(left, modules) {
            (full, right, *op)
        } else {
            (version_value(right, modules)?, left, swapped(*op)?)
        };
        let (major, minor) = version_tuple(tuple)?;
        // `sys.version_info == (3, 9)` is never true, but reads like a typo
        // for `[:2]`; leave it opaque.
        let read = match op {
            CmpOp::Lt | CmpOp::LtE | CmpOp::Gt | CmpOp::GtE => true,
            CmpOp::Eq | CmpOp::NotEq => !full,
            _ => false,
        };
        read.then_some(Self {
            op,
            major,
            minor,
            full,
        })
    }

    fn holds(&self, minor: u32) -> bool {
        let ordering = 3_u32
            .cmp(&self.major)
            .then_with(|| match self.minor {
                Some(bound) => minor.cmp(&bound),
                None => std::cmp::Ordering::Greater,
            })
            .then(if self.full {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            });
        match self.op {
            CmpOp::Lt => ordering.is_lt(),
            CmpOp::LtE => ordering.is_le(),
            CmpOp::Gt => ordering.is_gt(),
            CmpOp::GtE => ordering.is_ge(),
            CmpOp::Eq => ordering.is_eq(),
            _ => ordering.is_ne(),
        }
    }
}

/// `Some(true)` for `sys.version_info`, `Some(false)` for
/// `sys.version_info[:2]` (or `[0:2]`).
fn version_value(expr: &Expr, modules: &PlatformModules) -> Option<bool> {
    if is_sys_attribute(expr, modules, "version_info") {
        return Some(true);
    }
    let Expr::Subscript(subscript) = expr else {
        return None;
    };
    let Expr::Slice(slice) = &*subscript.slice else {
        return None;
    };
    let lower_is_zero = slice
        .lower
        .as_deref()
        .is_none_or(|lower| small_int(lower) == Some(0));
    (lower_is_zero
        && slice.step.is_none()
        && slice.upper.as_deref().and_then(small_int) == Some(2)
        && is_sys_attribute(&subscript.value, modules, "version_info"))
    .then_some(false)
}

/// `(3,)` or `(3, 9)`.
fn version_tuple(expr: &Expr) -> Option<(u32, Option<u32>)> {
    let Expr::Tuple(tuple) = expr else {
        return None;
    };
    match &*tuple.elts {
        [major] => Some((small_int(major)?, None)),
        [major, minor] => Some((small_int(major)?, Some(small_int(minor)?))),
        _ => None,
    }
}

/// A non-negative integer literal below [`LAST_MINOR`].
fn small_int(expr: &Expr) -> Option<u32> {
    let Expr::NumberLiteral(ExprNumberLiteral {
        value: Number::Int(value),
        ..
    }) = expr
    else {
        return None;
    };
    value
        .as_u8()
        .map(u32::from)
        .filter(|value| *value < LAST_MINOR)
}

/// The operator with its operands swapped: `(3, 9) <= v` is `v >= (3, 9)`.
fn swapped(op: CmpOp) -> Option<CmpOp> {
    match op {
        CmpOp::Lt => Some(CmpOp::Gt),
        CmpOp::LtE => Some(CmpOp::GtE),
        CmpOp::Gt => Some(CmpOp::Lt),
        CmpOp::GtE => Some(CmpOp::LtE),
        CmpOp::Eq | CmpOp::NotEq => Some(op),
        _ => None,
    }
}

/// The minors on which the branch tested by `test` (`None` for `else`) can
/// run, given the `earlier` tests of its chain; [`ALL_MINORS`] when none of
/// them reads `sys.version_info`.
pub(super) fn branch_minors(
    earlier: &[&Expr],
    test: Option<&Expr>,
    modules: &PlatformModules,
) -> Minors {
    if !modules
        .values()
        .any(|module| *module == PlatformModule::Sys)
    {
        return ALL_MINORS;
    }
    let reads_version = condition::leaves(earlier.iter().copied().chain(test))
        .into_iter()
        .any(|leaf| VersionTest::read(leaf, modules).is_some());
    if !reads_version {
        return ALL_MINORS;
    }
    (0..=LAST_MINOR)
        .filter(|&minor| {
            !condition::excluded(earlier, test, &|expr| {
                VersionTest::read(expr, modules)
                    .map_or(Leaf::Opaque, |read| Leaf::Fixed(read.holds(minor)))
            })
        })
        .fold(0, |minors, minor| minors | (1 << minor))
}

/// `minors` as inclusive runs; a run through the last bit ends at
/// `u32::MAX`.
pub(super) fn minor_runs(minors: Minors) -> Vec<[u32; 2]> {
    let mut runs: Vec<[u32; 2]> = Vec::new();
    for minor in (0..=LAST_MINOR).filter(|minor| minors & (1 << minor) != 0) {
        let high = if minor == LAST_MINOR { u32::MAX } else { minor };
        match runs.last_mut() {
            Some(run) if run[1] + 1 == minor => run[1] = high,
            _ => runs.push([minor, high]),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::platform_guard::platform_modules;

    fn runs(chain: &[&str], branch: usize) -> Vec<[u32; 2]> {
        let header = ruff_python_parser::parse_module("import os\nimport sys\n").expect("parse");
        let modules = platform_modules(None, None, header.suite());
        let parsed: Vec<_> = chain
            .iter()
            .map(|test| ruff_python_parser::parse_expression(test).expect("parse"))
            .collect();
        let tests: Vec<&Expr> = parsed
            .iter()
            .map(ruff_python_parser::Parsed::expr)
            .collect();
        let (earlier, test) = (
            &tests[..branch.min(tests.len())],
            tests.get(branch).copied(),
        );
        minor_runs(branch_minors(earlier, test, &modules))
    }

    const ALL: [u32; 2] = [0, u32::MAX];

    #[test]
    fn reads_version_info_comparisons_from_either_side() {
        for (test, body, orelse) in [
            ("sys.version_info >= (3, 9)", [9, u32::MAX], [0, 8]),
            ("sys.version_info < (3, 9)", [0, 8], [9, u32::MAX]),
            ("(3, 9) <= sys.version_info", [9, u32::MAX], [0, 8]),
            ("(3, 9) > sys.version_info", [0, 8], [9, u32::MAX]),
            ("(3, 9) < sys.version_info[:2]", [10, u32::MAX], [0, 9]),
            ("(3, 9) >= sys.version_info[:2]", [0, 9], [10, u32::MAX]),
            ("sys.version_info[:2] >= (3, 9)", [9, u32::MAX], [0, 8]),
            ("sys.version_info[0:2] < (3, 9)", [0, 8], [9, u32::MAX]),
            // The longer tuple is greater: 3.9.0 is past `(3, 9)`.
            ("sys.version_info > (3, 9)", [9, u32::MAX], [0, 8]),
            ("sys.version_info <= (3, 9)", [0, 8], [9, u32::MAX]),
            ("sys.version_info[:2] > (3, 9)", [10, u32::MAX], [0, 9]),
            ("sys.version_info[:2] <= (3, 9)", [0, 9], [10, u32::MAX]),
        ] {
            assert_eq!(runs(&[test], 0), [body], "{test}");
            assert_eq!(runs(&[test], 1), [orelse], "else of {test}");
        }
        assert_eq!(runs(&["sys.version_info[:2] == (3, 9)"], 0), [[9, 9]]);
        assert_eq!(runs(&["(3, 9) == sys.version_info[:2]"], 0), [[9, 9]]);
        // The last minor with a bit of its own is still read.
        assert_eq!(
            runs(&["sys.version_info >= (3, 126)"], 0),
            [[126, u32::MAX]]
        );
        assert_eq!(
            runs(&["sys.version_info[:2] != (3, 9)"], 0),
            [[0, 8], [10, u32::MAX]]
        );
    }

    #[test]
    fn major_only_and_python_2_tuples_fix_the_branch() {
        assert_eq!(
            runs(&["sys.version_info < (3,)"], 0),
            Vec::<[u32; 2]>::new()
        );
        assert_eq!(runs(&["sys.version_info < (3,)"], 1), [ALL]);
        assert_eq!(runs(&["sys.version_info >= (3, 0)"], 0), [ALL]);
        assert_eq!(
            runs(&["sys.version_info < (2, 7)"], 0),
            Vec::<[u32; 2]>::new()
        );
        assert_eq!(
            runs(&["sys.version_info >= (4,)"], 0),
            Vec::<[u32; 2]>::new()
        );
    }

    #[test]
    fn combines_through_boolean_operators_and_the_chain() {
        assert_eq!(
            runs(
                &["sys.version_info >= (3, 9) and sys.version_info < (3, 12)"],
                0
            ),
            [[9, 11]]
        );
        assert_eq!(runs(&["not sys.version_info >= (3, 9)"], 0), [[0, 8]]);
        // An opaque operand keeps every minor the other one allows.
        assert_eq!(runs(&["sys.version_info >= (3, 9) or flag"], 0), [ALL]);
        assert_eq!(
            runs(&["sys.version_info >= (3, 9) and flag"], 0),
            [[9, u32::MAX]]
        );
        let chain = ["sys.version_info >= (3, 11)", "sys.version_info >= (3, 9)"];
        assert_eq!(runs(&chain, 1), [[9, 10]]);
        assert_eq!(runs(&chain, 2), [[0, 8]]);
    }

    #[test]
    fn other_forms_are_opaque() {
        for test in [
            "sys.version_info.major >= 3",
            "sys.version_info[0] >= 3",
            "sys.version_info[1] >= 9",
            "sys.version_info >= (3, 9, 1)",
            "sys.version_info >= (3, minor)",
            "sys.version_info >= (3, 200)",
            "sys.version_info >= (3, 127)",
            "sys.platform >= (3, 9)",
            "os.version_info >= (3, 9)",
            "sys.version_info == (3, 9)",
            "sys.version_info[:3] >= (3, 9)",
            "sys.version_info[::2] >= (3, 9)",
            "(3, 8) < sys.version_info < (3, 12)",
            "version_info >= (3, 9)",
            "PY39",
        ] {
            assert_eq!(runs(&[test], 0), [ALL], "{test}");
            assert_eq!(runs(&[test], 1), [ALL], "else of {test}");
        }
    }

    #[test]
    fn a_shadowed_sys_is_opaque() {
        let module =
            ruff_python_parser::parse_module("import sys\nsys = object()\n").expect("parse");
        let modules = platform_modules(None, None, module.suite());
        let parsed =
            ruff_python_parser::parse_expression("sys.version_info >= (3, 9)").expect("parse");
        assert_eq!(
            branch_minors(&[], Some(parsed.expr()), &modules),
            ALL_MINORS
        );
    }
}
