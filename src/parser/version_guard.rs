//! `sys.version_info` comparisons in branch tests: the Python 3 minors a
//! branch can run on (#997).
//!
//! Only `sys.version_info` and `sys.version_info[:2]` compared once with a
//! tuple of one or two integer literals are read, from either side, with
//! `<` / `<=` / `>` / `>=` (and `==` / `!=` for `[:2]`), and the major
//! version (`sys.version_info[0]`, `sys.version_info.major`) compared with an
//! integer literal. A module-level name bound once to such a test
//! (`PY2 = sys.version_info[0] == 2`) reads as the test (#1272). Any other
//! test is an opaque leaf, so a branch it guards may run on every minor
//! (ADR 0005).

use std::collections::HashMap;

use ruff_python_ast::{CmpOp, Expr, ExprNumberLiteral, Number, Stmt};

use super::condition::{self, Leaf};
use super::platform_guard::{PlatformModule, PlatformModules, bound_names, is_sys_attribute};

/// Python 3 minors as bits: bit `n` is 3.`n`, and the last bit also stands
/// for every later minor, since no tuple read names one past it.
pub(super) type Minors = u128;

/// Every minor: no version test bounds the branch.
pub(super) const ALL_MINORS: Minors = Minors::MAX;

/// The last minor with a bit of its own.
const LAST_MINOR: u32 = Minors::BITS - 1;

/// Module-level names bound to a version test: per name, the minors on
/// which it is surely true and those on which it is surely false (#1272).
pub(super) type VersionNames = HashMap<String, (Minors, Minors)>;

/// One read comparison: `version_info <op> (major, minor)`, with the
/// version on the left.
struct VersionTest {
    op: CmpOp,
    major: u32,
    minor: Option<u32>,
    /// `sys.version_info` itself, which is longer than any tuple it is
    /// compared with: `(3, 9, 0, "final", 0) > (3, 9)`.
    full: bool,
    /// Only the major version is compared (`sys.version_info[0] == 2`).
    major_only: bool,
}

impl VersionTest {
    fn read(expr: &Expr, modules: &PlatformModules) -> Option<Self> {
        let Expr::Compare(compare) = expr else {
            return None;
        };
        let ([op], [left, right]) = (&*compare.ops, &*compare.operands) else {
            return None;
        };
        if let Some(test) = Self::read_major(*op, left, right, modules) {
            return Some(test);
        }
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
            major_only: false,
        })
    }

    /// `sys.version_info[0] <op> 2` or `sys.version_info.major <op> 2`, from
    /// either side.
    fn read_major(op: CmpOp, left: &Expr, right: &Expr, modules: &PlatformModules) -> Option<Self> {
        let (major, op) = if is_major_value(left, modules) {
            (small_int(right)?, op)
        } else if is_major_value(right, modules) {
            (small_int(left)?, swapped(op)?)
        } else {
            return None;
        };
        swapped(op).map(|_| Self {
            op,
            major,
            minor: None,
            full: false,
            major_only: true,
        })
    }

    fn holds(&self, minor: u32) -> bool {
        let ordering = 3_u32
            .cmp(&self.major)
            .then_with(|| match self.minor {
                _ if self.major_only => std::cmp::Ordering::Equal,
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

/// `sys.version_info[0]` or `sys.version_info.major`.
fn is_major_value(expr: &Expr, modules: &PlatformModules) -> bool {
    match expr {
        Expr::Attribute(attribute) => {
            attribute.attr.as_str() == "major"
                && is_sys_attribute(&attribute.value, modules, "version_info")
        },
        Expr::Subscript(subscript) => {
            small_int(&subscript.slice) == Some(0)
                && is_sys_attribute(&subscript.value, modules, "version_info")
        },
        _ => false,
    }
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

/// What `expr`, a leaf, is on `minor`: a read comparison's value, or a bound
/// name's where its test fixes one.
fn version_leaf(expr: &Expr, minor: u32, modules: &PlatformModules, names: &VersionNames) -> Leaf {
    if let Expr::Name(name) = expr {
        return match names.get(name.id.as_str()) {
            Some((true_on, _)) if true_on & (1 << minor) != 0 => Leaf::Fixed(true),
            Some((_, false_on)) if false_on & (1 << minor) != 0 => Leaf::Fixed(false),
            _ => Leaf::Opaque,
        };
    }
    VersionTest::read(expr, modules).map_or(Leaf::Opaque, |read| Leaf::Fixed(read.holds(minor)))
}

fn reads_version(exprs: &[&Expr], modules: &PlatformModules, names: &VersionNames) -> bool {
    let sys_bound = modules
        .values()
        .any(|module| *module == PlatformModule::Sys);
    (sys_bound || !names.is_empty())
        && condition::leaves(exprs.iter().copied())
            .into_iter()
            .any(|leaf| match leaf {
                Expr::Name(name) => names.contains_key(name.id.as_str()),
                leaf => sys_bound && VersionTest::read(leaf, modules).is_some(),
            })
}

/// The minors on which the branch tested by `test` (`None` for `else`) can
/// run, given the `earlier` tests of its chain; [`ALL_MINORS`] when none of
/// them reads `sys.version_info` or a name in `names`.
pub(super) fn branch_minors(
    earlier: &[&Expr],
    test: Option<&Expr>,
    modules: &PlatformModules,
    names: &VersionNames,
) -> Minors {
    let tests: Vec<&Expr> = earlier.iter().copied().chain(test).collect();
    if !reads_version(&tests, modules, names) {
        return ALL_MINORS;
    }
    (0..=LAST_MINOR)
        .filter(|&minor| {
            !condition::excluded(earlier, test, &|expr| {
                version_leaf(expr, minor, modules, names)
            })
        })
        .fold(0, |minors, minor| minors | (1 << minor))
}

/// The names the module-level statements `stmts` bind exactly once, by a
/// plain `name = <test>` whose value reads a version (`PY2 =
/// sys.version_info[0] == 2`, `PY3 = not PY2`), with the minors the test is
/// true and false on (#1272). A name bound anywhere else in the module scope
/// may hold something else by the time a test reads it, so it is left out.
pub(super) fn version_names(stmts: &[Stmt], modules: &PlatformModules) -> VersionNames {
    let mut names = VersionNames::new();
    let bound = bound_names(None, stmts);
    for stmt in stmts {
        let Stmt::Assign(assign) = stmt else {
            continue;
        };
        let [Expr::Name(target)] = &*assign.targets else {
            continue;
        };
        if bound.get(target.id.as_str()) != Some(&1)
            || !reads_version(&[&assign.value], modules, &names)
        {
            continue;
        }
        let (mut true_on, mut false_on) = (0, 0);
        for minor in 0..=LAST_MINOR {
            let leaf = |expr: &Expr| version_leaf(expr, minor, modules, &names);
            match condition::fixed(&assign.value, &leaf) {
                Some(true) => true_on |= 1 << minor,
                Some(false) => false_on |= 1 << minor,
                None => {},
            }
        }
        names.insert(target.id.to_string(), (true_on, false_on));
    }
    names
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
        minor_runs(branch_minors(earlier, test, &modules, &VersionNames::new()))
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
            "sys.version_info[1] >= 9",
            "sys.version_info[0] >= minor",
            "sys.version_info[0] in (2, 3)",
            "sys.version_info.minor >= 9",
            "os.version_info[0] == 2",
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
            branch_minors(&[], Some(parsed.expr()), &modules, &VersionNames::new()),
            ALL_MINORS
        );
    }

    /// #1272: six's `PY2 = sys.version_info[0] == 2`.
    #[test]
    fn reads_the_major_version() {
        let none = Vec::<[u32; 2]>::new();
        for (test, body, orelse) in [
            ("sys.version_info[0] == 2", &none, &vec![ALL]),
            ("sys.version_info[0] == 3", &vec![ALL], &none),
            ("sys.version_info.major == 3", &vec![ALL], &none),
            ("3 == sys.version_info[0]", &vec![ALL], &none),
            ("sys.version_info[0] >= 3", &vec![ALL], &none),
            ("sys.version_info[0] < 3", &none, &vec![ALL]),
            ("2 < sys.version_info.major", &vec![ALL], &none),
            ("sys.version_info[0] != 2", &vec![ALL], &none),
            ("sys.version_info.major > 3", &none, &vec![ALL]),
        ] {
            assert_eq!(&runs(&[test], 0), body, "{test}");
            assert_eq!(&runs(&[test], 1), orelse, "else of {test}");
        }
    }

    fn names_runs(source: &str, test: &str, branch: usize) -> Vec<[u32; 2]> {
        let module = ruff_python_parser::parse_module(source).expect("parse");
        let modules = platform_modules(None, None, module.suite());
        let names = version_names(module.suite(), &modules);
        let parsed = ruff_python_parser::parse_expression(test).expect("parse");
        let (earlier, test) = if branch == 0 {
            (vec![], Some(parsed.expr()))
        } else {
            (vec![parsed.expr()], None)
        };
        minor_runs(branch_minors(&earlier, test, &modules, &names))
    }

    #[test]
    fn names_bound_once_to_a_version_test_read_as_it() {
        let six = "import sys\nPY2 = sys.version_info[0] == 2\nPY3 = not PY2\nPY34 = sys.version_info[0:2] >= (3, 4)\nMAYBE = sys.version_info >= (3, 9) and flag\n";
        let none = Vec::<[u32; 2]>::new();
        assert_eq!(names_runs(six, "PY2", 0), none);
        assert_eq!(names_runs(six, "PY2", 1), [ALL]);
        assert_eq!(names_runs(six, "PY3", 1), none);
        assert_eq!(names_runs(six, "not PY3", 0), none);
        assert_eq!(names_runs(six, "PY34", 0), [[4, u32::MAX]]);
        assert_eq!(names_runs(six, "PY34", 1), [[0, 3]]);
        // `flag` is unknown on 3.9+, so only the else of 3.8- is fixed.
        assert_eq!(names_runs(six, "MAYBE", 0), [[9, u32::MAX]]);
        assert_eq!(names_runs(six, "MAYBE", 1), [ALL]);
        assert_eq!(names_runs(six, "OTHER", 0), [ALL]);
        for rebound in [
            "PY2 = False",
            "PY2 += 1",
            "for PY2 in x:\n    pass",
            "def PY2():\n    pass",
            "import PY2",
            "if x:\n    PY2 = True",
        ] {
            let source = format!("import sys\nPY2 = sys.version_info[0] == 2\n{rebound}\n");
            assert_eq!(names_runs(&source, "PY2", 0), [ALL], "{rebound}");
        }
        // Only a plain assignment of one name binds a test.
        for source in [
            "import sys\nPY2: bool = sys.version_info[0] == 2\n",
            "import sys\nPY2 = OTHER = sys.version_info[0] == 2\n",
            "PY2 = sys.version_info[0] == 2\n",
        ] {
            assert_eq!(names_runs(source, "PY2", 0), [ALL], "{source}");
        }
    }
}
