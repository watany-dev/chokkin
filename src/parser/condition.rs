//! What an `if` / `elif` / `else` chain proves about when a branch runs.
//!
//! A guard rules a branch out when some fact makes it never run:
//! `TYPE_CHECKING` is false whenever the code runs, `find_spec("x")` is false
//! without `x`, and some platform makes `sys.platform == "win32"` false.
//! [`excluded`] reads that fact through `not`, `and`, `or` and the tests of
//! the earlier branches. Any other sub-expression is opaque: it may take
//! either value, so it never helps rule a branch out, and a condition the
//! guard cannot read leaves the import a runtime requirement (ADR 0005).

use ruff_python_ast::{BoolOp, Expr, UnaryOp};

/// What a guard knows about one operand that is not `not`, `and` or `or`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Leaf {
    /// Either value, and the guard cannot pick one.
    Opaque,
    /// Always this value where the guard applies (`TYPE_CHECKING` at runtime).
    Fixed(bool),
    /// Whichever value the guard needs (some platform fails `sys.platform == "x"`).
    Free,
}

/// Whether the leaves the guard knows can make `expr` evaluate to `value`
/// whatever the opaque ones are. Exact when no leaf occurs twice; a repeated
/// opaque leaf only makes the answer `false` more often.
fn can(expr: &Expr, value: bool, leaf: &dyn Fn(&Expr) -> Leaf) -> bool {
    match expr {
        Expr::UnaryOp(unary) if unary.op == UnaryOp::Not => can(&unary.operand, !value, leaf),
        // `and` is true when every operand is and false when one is; `or` the
        // other way round.
        Expr::BoolOp(bool_op) if (bool_op.op == BoolOp::And) == value => bool_op
            .values
            .iter()
            .all(|operand| can(operand, value, leaf)),
        Expr::BoolOp(bool_op) => bool_op
            .values
            .iter()
            .any(|operand| can(operand, value, leaf)),
        _ => match leaf(expr) {
            Leaf::Opaque => false,
            Leaf::Fixed(fixed) => fixed == value,
            Leaf::Free => true,
        },
    }
}

/// Whether the branch tested by `test` (`None` for `else`) never runs where
/// the guard applies: an `earlier` test of its chain can be true there, or its
/// own can be false.
pub(super) fn excluded(
    earlier: &[&Expr],
    test: Option<&Expr>,
    leaf: &dyn Fn(&Expr) -> Leaf,
) -> bool {
    earlier.iter().any(|test| can(test, true, leaf))
        || test.is_some_and(|test| can(test, false, leaf))
}

/// The operands of `exprs` that are not `not`, `and` or `or`.
pub(super) fn leaves<'a>(exprs: impl IntoIterator<Item = &'a Expr>) -> Vec<&'a Expr> {
    let mut stack: Vec<&Expr> = exprs.into_iter().collect();
    let mut out = Vec::new();
    while let Some(expr) = stack.pop() {
        match expr {
            Expr::UnaryOp(unary) if unary.op == UnaryOp::Not => stack.push(&unary.operand),
            Expr::BoolOp(bool_op) => stack.extend(&bool_op.values),
            _ => out.push(expr),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::parser::ParsedModule;
    use crate::parser::lines::LineIndex;
    use crate::parser::visit::ModuleVisitor;
    use crate::sources::{FileContext, LayoutInfo};

    fn visit_source(source: &str) -> ParsedModule {
        let module = ruff_python_parser::parse_module(source).expect("parse");
        let layout = LayoutInfo::default();
        let lines = LineIndex::new(source);
        let mut visitor = ModuleVisitor::new("mod.py", &layout, FileContext::Runtime, &lines);
        visitor.visit_module(source, module.suite());
        visitor.into_parsed()
    }

    fn optional_by_module(source: &str) -> Vec<(String, bool)> {
        visit_source(source)
            .imports
            .into_iter()
            .map(|import| (import.module, import.optional))
            .collect()
    }

    /// The `else` of a negated try flag, `is_x` predicate or sniffio check
    /// runs only once the package loaded; the forms the reference model
    /// below does not generate (#861).
    #[test]
    fn negated_flags_and_predicates_guard_the_else_branch() {
        let got = optional_by_module(
            "import sniffio
try:
    import yaml
    has_yaml = True
except ImportError:
    has_yaml = False
if not has_yaml:
    import json_fallback
else:
    import yaml.constructor
if not is_psycopg3:
    import psycopg2
else:
    import psycopg.adapt
if not (sniffio.current_async_library() == \"trio\"):
    import anyio
else:
    import trio.lowlevel
if is_psycopg3 and settings.DEBUG:
    import psycopg.rows
    import other
",
        );
        let optional: Vec<&str> = got
            .iter()
            .filter(|(_, optional)| *optional)
            .map(|(module, _)| module.as_str())
            .collect();
        assert_eq!(
            optional,
            [
                "yaml",
                "yaml.constructor",
                "psycopg.adapt",
                "trio.lowlevel",
                "psycopg.rows",
            ]
        );
    }

    /// A flag means nothing inside a function, where a local of the same
    /// name may shadow it.
    #[test]
    fn function_level_flags_guard_nothing() {
        let got = optional_by_module(
            "try:\n    import yaml\n    has_yaml = True\nexcept ImportError:\n    has_yaml = False\n\
             def load():\n    if not has_yaml:\n        return None\n    else:\n        import yaml.nodes\n",
        );
        assert_eq!(
            got,
            [("yaml".to_owned(), true), ("yaml.nodes".to_owned(), false)]
        );
    }

    /// `-x`, `~x` and `+x` are opaque operands, not a negation of `x`.
    #[test]
    fn other_unary_operators_are_leaves() {
        use ruff_python_ast::Expr;

        use super::{Leaf, excluded, leaves};

        let known = |expr: &Expr| {
            if expr.is_name_expr() {
                Leaf::Fixed(true)
            } else {
                Leaf::Opaque
            }
        };
        for source in ["-x", "~x", "+x"] {
            let parsed = ruff_python_parser::parse_expression(source).expect("parse");
            let test = parsed.expr();
            assert!(!excluded(&[], Some(test), &known), "{source}");
            assert!(!excluded(&[test], None, &known), "{source}");
            assert_eq!(leaves([test]), [test], "{source}");
        }
    }

    /// Guards built from `not`, `and`, `or` and `elif` / `else` against a
    /// reference model that tries every value of the facts the code cannot
    /// know (#861).
    mod props {
        use std::fmt::Write as _;

        use proptest::prelude::*;

        use super::visit_source;
        use crate::parser::ImportContext;

        /// One leaf of a generated condition.
        #[derive(Clone, Copy, Debug)]
        enum Atom {
            /// `TYPE_CHECKING`: false whenever the code runs.
            TypeChecking,
            /// A platform test (#957): true on some platform, false on another.
            Platform,
            /// A flag, `sys.version_info` comparison or call the analysis cannot read.
            Opaque,
            /// `find_spec("x")` (or `is not None`): false without the package.
            Installed,
            /// `find_spec("x") is None`: true without the package.
            Missing,
            /// `__name__ == "__main__"`: false when the file is imported.
            Main,
            /// `"x" in sys.modules` for one of [`ROOTS`]: false until `x` is loaded.
            Loaded(usize),
        }

        #[derive(Clone, Debug)]
        enum Cond {
            Atom(Atom),
            Not(Box<Self>),
            And(Vec<Self>),
            Or(Vec<Self>),
        }

        #[derive(Clone, Debug)]
        enum Block {
            /// `import <ROOTS[i]>.<unique>`.
            Import(usize),
            /// `if` and `elif` branches, then the `else` body.
            If(Vec<(Cond, Vec<Self>)>, Option<Vec<Self>>),
        }

        const ROOTS: [&str; 3] = ["zzma", "zzmb", "zzmc"];

        const HEADER: &str = "import importlib.util\nimport os\nimport platform\nimport sys\nimport typing\n\
                              from typing import TYPE_CHECKING\n";

        fn atom() -> impl Strategy<Value = Atom> {
            prop_oneof![
                Just(Atom::TypeChecking),
                Just(Atom::Platform),
                Just(Atom::Opaque),
                Just(Atom::Installed),
                Just(Atom::Missing),
                Just(Atom::Main),
                (0..ROOTS.len()).prop_map(Atom::Loaded),
            ]
        }

        fn cond() -> impl Strategy<Value = Cond> {
            atom()
                .prop_map(Cond::Atom)
                .prop_recursive(3, 4, 3, |inner| {
                    prop_oneof![
                        inner.clone().prop_map(|cond| Cond::Not(Box::new(cond))),
                        prop::collection::vec(inner.clone(), 2..4).prop_map(Cond::And),
                        prop::collection::vec(inner, 2..4).prop_map(Cond::Or),
                    ]
                })
                // The reference model tries every value of every leaf.
                .prop_filter("at most 5 leaves", |cond| cond.leaf_count() <= 5)
        }

        impl Cond {
            fn leaf_count(&self) -> usize {
                match self {
                    Self::Atom(_) => 1,
                    Self::Not(cond) => cond.leaf_count(),
                    Self::And(conds) | Self::Or(conds) => conds.iter().map(Self::leaf_count).sum(),
                }
            }
        }

        fn block() -> impl Strategy<Value = Block> {
            (0..ROOTS.len())
                .prop_map(Block::Import)
                .prop_recursive(2, 8, 3, |inner| {
                    let body = prop::collection::vec(inner, 1..3);
                    (
                        prop::collection::vec((cond(), body.clone()), 1..3),
                        proptest::option::of(body),
                    )
                        .prop_map(|(branches, orelse)| Block::If(branches, orelse))
                })
        }

        /// A condition whose leaves are numbered occurrences, so every
        /// `Platform` / `Opaque` leaf is a fact of its own.
        #[derive(Clone, Debug)]
        enum Fact {
            Leaf(usize),
            Not(Box<Self>),
            All(Vec<Self>),
            Any(Vec<Self>),
        }

        impl Fact {
            fn holds(&self, value: &dyn Fn(usize) -> bool) -> bool {
                match self {
                    Self::Leaf(id) => value(*id),
                    Self::Not(fact) => !fact.holds(value),
                    Self::All(facts) => facts.iter().all(|fact| fact.holds(value)),
                    Self::Any(facts) => facts.iter().any(|fact| fact.holds(value)),
                }
            }

            fn leaves(&self, out: &mut Vec<usize>) {
                match self {
                    Self::Leaf(id) => out.push(*id),
                    Self::Not(fact) => fact.leaves(out),
                    Self::All(facts) | Self::Any(facts) => {
                        for fact in facts {
                            fact.leaves(out);
                        }
                    },
                }
            }
        }

        /// What the reference model needs of a rendered program.
        #[derive(Default)]
        struct Program {
            source: String,
            atoms: Vec<Atom>,
            /// Per import in source order: its root, and per enclosing branch
            /// the condition under which that branch runs. The branches use
            /// disjoint leaves, so the import never runs exactly when one of
            /// them never runs.
            imports: Vec<(usize, Vec<Fact>)>,
        }

        /// Comments, blank lines and line breaks inside parentheses; `None`
        /// renders without any.
        struct Noise<'a> {
            choices: &'a [u8],
            at: usize,
        }

        impl Noise<'_> {
            fn next(&mut self, kinds: u8) -> u8 {
                let Some(choice) = self.choices.get(self.at % self.choices.len().max(1)) else {
                    return 0;
                };
                self.at += 1;
                choice % kinds
            }
        }

        fn atom_text(atom: Atom, id: usize) -> String {
            match atom {
                Atom::TypeChecking if id.is_multiple_of(2) => "TYPE_CHECKING".to_owned(),
                Atom::TypeChecking => "typing.TYPE_CHECKING".to_owned(),
                Atom::Platform => match id % 6 {
                    0 => format!("sys.platform == \"p{id}\""),
                    1 => format!("sys.platform != \"p{id}\""),
                    2 => format!("\"p{id}\" <= sys.platform"),
                    3 => format!("sys.platform.startswith(\"p{id}\")"),
                    4 => format!("os.name in (\"p{id}\", \"q{id}\")"),
                    _ => format!("platform.system() == \"P{id}\""),
                },
                Atom::Opaque => match id % 3 {
                    0 => format!("zzflag{id}"),
                    1 => format!("sys.version_info >= (3, {id})"),
                    _ => format!("os.environ.get(\"Z{id}\")"),
                },
                Atom::Installed if id.is_multiple_of(2) => {
                    format!("importlib.util.find_spec(\"zzq{id}\")")
                },
                Atom::Installed => format!("importlib.util.find_spec(\"zzq{id}\") is not None"),
                Atom::Missing => format!("importlib.util.find_spec(\"zzq{id}\") is None"),
                Atom::Main => "__name__ == \"__main__\"".to_owned(),
                Atom::Loaded(root) => format!("\"{}\" in sys.modules", ROOTS[root]),
            }
        }

        impl Program {
            fn render_cond(&mut self, cond: &Cond, noise: &mut Noise<'_>) -> (String, Fact) {
                match cond {
                    Cond::Atom(atom) => {
                        let id = self.atoms.len();
                        self.atoms.push(*atom);
                        (atom_text(*atom, id), Fact::Leaf(id))
                    },
                    Cond::Not(cond) => {
                        let (text, fact) = self.render_cond(cond, noise);
                        (format!("not ({text})"), Fact::Not(Box::new(fact)))
                    },
                    Cond::And(conds) => {
                        let (text, facts) = self.render_join(conds, "and", noise);
                        (text, Fact::All(facts))
                    },
                    Cond::Or(conds) => {
                        let (text, facts) = self.render_join(conds, "or", noise);
                        (text, Fact::Any(facts))
                    },
                }
            }

            fn render_join(
                &mut self,
                conds: &[Cond],
                op: &str,
                noise: &mut Noise<'_>,
            ) -> (String, Vec<Fact>) {
                let mut text = String::new();
                let mut facts = Vec::new();
                for (i, cond) in conds.iter().enumerate() {
                    if i > 0 {
                        text.push_str(match noise.next(3) {
                            0 => " ",
                            1 => "\n        ",
                            _ => "  # why\n",
                        });
                        text.push_str(op);
                        text.push(' ');
                    }
                    let (part, fact) = self.render_cond(cond, noise);
                    let _ = write!(text, "({part})");
                    facts.push(fact);
                }
                (text, facts)
            }

            fn render_line(&mut self, indent: usize, line: &str, noise: &mut Noise<'_>) {
                match noise.next(4) {
                    1 => self.source.push('\n'),
                    2 => {
                        let _ = writeln!(self.source, "{:indent$}# note", "");
                    },
                    3 => self.source.push_str("# flush-left note\n"),
                    _ => {},
                }
                let comment = if noise.next(2) == 1 {
                    "  # trailing"
                } else {
                    ""
                };
                let _ = writeln!(self.source, "{:indent$}{line}{comment}", "");
            }

            fn render_test(
                &mut self,
                indent: usize,
                keyword: &str,
                cond: &Cond,
                noise: &mut Noise<'_>,
            ) -> Fact {
                let (text, fact) = self.render_cond(cond, noise);
                // A line break between operands needs the parentheses.
                let line = if noise.next(2) == 1 || text.contains('\n') {
                    format!("{keyword} (  # open\n    {text}\n):")
                } else {
                    format!("{keyword} {text}:")
                };
                self.render_line(indent, &line, noise);
                fact
            }

            fn render_body(
                &mut self,
                indent: usize,
                path: &[Fact],
                body: &[Block],
                noise: &mut Noise<'_>,
            ) {
                for block in body {
                    let (branches, orelse) = match block {
                        Block::Import(root) => {
                            let id = self.imports.len();
                            self.imports.push((*root, path.to_vec()));
                            self.render_line(
                                indent,
                                &format!("import {}.i{id}", ROOTS[*root]),
                                noise,
                            );
                            continue;
                        },
                        Block::If(branches, orelse) => (branches, orelse),
                    };
                    let keywords = std::iter::once("if").chain(std::iter::repeat("elif"));
                    let mut failed = Vec::new();
                    for ((cond, body), keyword) in branches.iter().zip(keywords) {
                        let fact = self.render_test(indent, keyword, cond, noise);
                        let mut runs = failed.clone();
                        runs.push(fact.clone());
                        let mut inner = path.to_vec();
                        inner.push(Fact::All(runs));
                        self.render_body(indent + 4, &inner, body, noise);
                        failed.push(Fact::Not(Box::new(fact)));
                    }
                    if let Some(body) = orelse {
                        self.render_line(indent, "else:", noise);
                        let mut inner = path.to_vec();
                        inner.push(Fact::All(failed));
                        self.render_body(indent + 4, &inner, body, noise);
                    }
                }
            }

            fn new(blocks: &[Block], choices: &[u8]) -> Self {
                let mut program = Self {
                    source: HEADER.to_owned(),
                    ..Self::default()
                };
                let mut noise = Noise { choices, at: 0 };
                program.render_body(0, &[], blocks, &mut noise);
                program
            }

            /// Whether `branch` never runs: some value of the `chosen` facts
            /// makes it false whatever the facts that are neither `fixed` nor
            /// chosen are, every leaf tried.
            fn never_runs(
                &self,
                branch: &Fact,
                fixed: &dyn Fn(Atom) -> Option<bool>,
                chosen: &dyn Fn(Atom) -> bool,
            ) -> bool {
                let mut leaves = Vec::new();
                branch.leaves(&mut leaves);
                let free: Vec<usize> = leaves
                    .into_iter()
                    .filter(|&id| fixed(self.atoms[id]).is_none())
                    .collect();
                let (picked, unknown): (Vec<usize>, Vec<usize>) =
                    free.into_iter().partition(|&id| chosen(self.atoms[id]));
                let bit = |ids: &[usize], bits: u32, id: usize| {
                    ids.iter()
                        .position(|&leaf| leaf == id)
                        .is_some_and(|at| (bits >> at) & 1 == 1)
                };
                let value = |picks: u32, unknowns: u32, id: usize| {
                    fixed(self.atoms[id])
                        .unwrap_or_else(|| bit(&picked, picks, id) || bit(&unknown, unknowns, id))
                };
                (0..1u32 << picked.len()).any(|picks| {
                    (0..1u32 << unknown.len())
                        .all(|unknowns| !branch.holds(&|id| value(picks, unknowns, id)))
                })
            }

            fn never_runs_on_path(
                &self,
                path: &[Fact],
                fixed: &dyn Fn(Atom) -> Option<bool>,
                chosen: &dyn Fn(Atom) -> bool,
            ) -> bool {
                path.iter()
                    .any(|branch| self.never_runs(branch, fixed, chosen))
            }

            /// `(context, optional, platform_guarded)` per import. Each flag
            /// reads only its own facts: under `TYPE_CHECKING` the context
            /// already says the import never runs, so `optional` and
            /// `platform_guarded` leave it unknown and keep a type-only import
            /// reported as missing.
            fn expected(&self) -> Vec<(ImportContext, bool, bool)> {
                let runtime = |atom: Atom| matches!(atom, Atom::TypeChecking).then_some(false);
                let unknown = |_: Atom| None;
                let none = |_: Atom| false;
                self.imports
                    .iter()
                    .map(|(root, path)| {
                        let type_only = self.never_runs_on_path(path, &runtime, &none);
                        let main_only = self.never_runs_on_path(
                            path,
                            &|atom| match atom {
                                Atom::TypeChecking | Atom::Main => Some(false),
                                _ => None,
                            },
                            &none,
                        );
                        let context = if type_only {
                            ImportContext::Type
                        } else if main_only {
                            ImportContext::Dev
                        } else {
                            ImportContext::Runtime
                        };
                        let optional = self.never_runs_on_path(
                            path,
                            &|atom| match atom {
                                Atom::Installed => Some(false),
                                Atom::Missing => Some(true),
                                Atom::Loaded(at) if at == *root => Some(false),
                                _ => None,
                            },
                            &none,
                        );
                        let platform = self.never_runs_on_path(path, &unknown, &|atom| {
                            matches!(atom, Atom::Platform)
                        });
                        (context, optional, platform)
                    })
                    .collect()
            }
        }

        fn classified(source: &str) -> Vec<(ImportContext, bool, bool)> {
            visit_source(source)
                .imports
                .iter()
                .filter(|import| import.module.starts_with("zzm"))
                .map(|import| (import.context, import.optional, import.platform_guarded))
                .collect()
        }

        proptest! {
            /// An import is type-only, `__main__`-only, optional or
            /// platform-guarded exactly when its branches prove it, so the
            /// classification never claims a runtime requirement the
            /// conditions rule out (#861). Every leaf occurs once, so the
            /// reference model's exhaustive answer is the exact one.
            #[test]
            fn guard_classification_matches_reference_model(blocks in prop::collection::vec(block(), 1..4)) {
                let program = Program::new(&blocks, &[]);
                prop_assert_eq!(
                    classified(&program.source),
                    program.expected(),
                    "{}",
                    program.source
                );
            }

            /// Comments, blank lines and line breaks inside parentheses
            /// change no classification.
            #[test]
            fn guard_classification_ignores_layout_noise(
                blocks in prop::collection::vec(block(), 1..4),
                choices in prop::collection::vec(any::<u8>(), 1..16),
            ) {
                let plain = Program::new(&blocks, &[]);
                let noisy = Program::new(&blocks, &choices);
                prop_assert_eq!(
                    classified(&noisy.source),
                    classified(&plain.source),
                    "{}",
                    noisy.source
                );
            }
        }

        #[test]
        fn version_branches_are_runtime_imports() {
            let parsed = visit_source(
                "import sys\nif sys.version_info >= (3, 11):\n    import tomllib\nelse:\n    import tomli\n",
            );
            let got: Vec<_> = parsed
                .imports
                .iter()
                .filter(|import| import.module.starts_with("toml"))
                .map(|import| {
                    (
                        import.module.as_str(),
                        import.context,
                        import.optional,
                        import.platform_guarded,
                    )
                })
                .collect();
            assert_eq!(
                got,
                [
                    ("tomllib", ImportContext::Runtime, false, false),
                    ("tomli", ImportContext::Runtime, false, false),
                ]
            );
        }
    }
}
