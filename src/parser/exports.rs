//! `__all__` export list extraction.

use ruff_python_ast::{self as ast, ExceptHandler, Expr, Operator, Stmt};
use ruff_text_size::Ranged;

use super::lines::LineIndex;
use super::module_guard::string_literal;
use super::types::{Exports, ParseDiagnostic, ParseSeverity};

/// Read `__all__` and warn about each update that could not be read.
///
/// A literal assignment replaces the list; a later literal `+=`, `.extend`,
/// `.append`, `.insert` or `__all__ + [...]` adds to it (#567, #876). A
/// block's paths are read one by one and joined as the union of their names
/// (#906). Any other update leaves the result [`Exports::Partial`] or
/// [`Exports::Unknown`] (ADR 0005).
pub(super) fn extract_exports(
    stmts: &[Stmt],
    lines: &LineIndex,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Exports {
    let mut state = State::default();
    Reader { lines, diagnostics }.body(stmts, Scope::Module, &mut state);
    let State {
        mut exports,
        unbound,
    } = state;
    if let Some(line) = unbound {
        diagnostics.push(ParseDiagnostic {
            line,
            message: "`__all__` is assigned on only some paths through this block, so it may be \
                      unset"
                .to_owned(),
            severity: ParseSeverity::Warning,
        });
        exports.lose();
    }
    exports
}

/// What `__all__` holds after the statements read so far, over every path
/// through them.
#[derive(Clone, Default)]
struct State {
    exports: Exports,
    /// The line of the block after which `__all__` may still be unbound on
    /// some path, while it holds [`Self::exports`] on the others. Only set
    /// with a read list.
    unbound: Option<u32>,
}

impl State {
    /// The state after either `self` or `other`, where the paths of the
    /// block at `line` meet. Names are united: an export that only one path
    /// adds is public too.
    fn join(self, other: Self, line: u32) -> Self {
        let unbound = self.unbound.or(other.unbound);
        let (exports, unbound) = match (self.exports, other.exports) {
            (Exports::Unknown, _) | (_, Exports::Unknown) => (Exports::Unknown, None),
            (Exports::Absent, Exports::Absent) => (Exports::Absent, None),
            (Exports::Absent, read) | (read, Exports::Absent) => (read, unbound.or(Some(line))),
            (Exports::Known(mut names), Exports::Known(more)) => {
                unite(&mut names, more);
                (Exports::Known(names), unbound)
            },
            (
                Exports::Known(mut names) | Exports::Partial(mut names),
                Exports::Known(more) | Exports::Partial(more),
            ) => {
                unite(&mut names, more);
                (Exports::Partial(names), unbound)
            },
        };
        Self { exports, unbound }
    }
}

fn unite(names: &mut Vec<String>, more: Vec<String>) {
    for name in more {
        if !names.contains(&name) {
            names.push(name);
        }
    }
}

/// Where a statement runs, which decides what its update can tell. Ordered
/// from most to least readable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    /// Module level, blocks included: each path through it runs once, in
    /// order.
    Module,
    /// Function bodies with `global __all__`: they run only when called, so
    /// no update there is read.
    Opaque,
    /// A class body, or a function body without `global __all__`: binding
    /// `__all__` there makes a local name, so only mutations of the module
    /// list count, and like [`Self::Opaque`] none of them is read.
    Local,
}

struct Reader<'a> {
    lines: &'a LineIndex,
    diagnostics: &'a mut Vec<ParseDiagnostic>,
}

impl Reader<'_> {
    fn body(&mut self, stmts: &[Stmt], scope: Scope, state: &mut State) {
        for stmt in stmts {
            self.stmt(stmt, scope, state);
        }
    }

    fn stmt(&mut self, stmt: &Stmt, scope: Scope, state: &mut State) {
        self.nested(stmt, scope, state);
        if scope == Scope::Local && binds_all(stmt) {
            return;
        }
        let Some(update) = all_update(stmt) else {
            return;
        };
        if apply(update, scope, state).is_err() {
            self.diagnostics.push(ParseDiagnostic {
                line: self.lines.line(stmt.start()),
                message: "unsupported `__all__` assignment form".to_owned(),
                severity: ParseSeverity::Warning,
            });
        }
    }

    /// Read the statements nested in `stmt`, joining the paths through it.
    fn nested(&mut self, stmt: &Stmt, scope: Scope, state: &mut State) {
        let line = self.lines.line(stmt.start());
        match stmt {
            Stmt::If(stmt) => {
                let before = state.clone();
                self.body(&stmt.body, scope, state);
                for clause in &stmt.elif_else_clauses {
                    let mut branch = before.clone();
                    self.body(&clause.body, scope, &mut branch);
                    *state = std::mem::take(state).join(branch, line);
                }
                if stmt
                    .elif_else_clauses
                    .last()
                    .is_none_or(|clause| clause.test.is_some())
                {
                    *state = before.join(std::mem::take(state), line);
                }
            },
            Stmt::Try(stmt) => {
                // A handler starts wherever the body may have raised.
                let raised = self.cut_short(&stmt.body, scope, state, line);
                let mut handled: Option<State> = None;
                for ExceptHandler::ExceptHandler(handler) in &stmt.handlers {
                    let mut branch = raised.clone();
                    self.body(&handler.body, scope, &mut branch);
                    handled = Some(match handled {
                        Some(handled) => handled.join(branch, line),
                        None => branch,
                    });
                }
                self.body(&stmt.orelse, scope, state);
                if let Some(handled) = handled {
                    *state = handled.join(std::mem::take(state), line);
                }
                self.body(&stmt.finalbody, scope, state);
            },
            // The context manager may suppress an exception and skip the
            // rest of the body (`suppress(ImportError)`).
            Stmt::With(stmt) => *state = self.cut_short(&stmt.body, scope, state, line),
            Stmt::For(ast::StmtFor { body, orelse, .. })
            | Stmt::While(ast::StmtWhile { body, orelse, .. }) => {
                let mut ran = state.clone();
                self.body(body, scope.max(Scope::Opaque), &mut ran);
                self.body(orelse, scope.max(Scope::Opaque), &mut ran);
                *state = std::mem::take(state).join(ran, line);
            },
            Stmt::Match(stmt) => {
                let mut ran = state.clone();
                for case in &stmt.cases {
                    self.body(&case.body, scope.max(Scope::Opaque), &mut ran);
                }
                *state = std::mem::take(state).join(ran, line);
            },
            // An `export` decorator appending `fn.__name__` is the usual case.
            // `global` is read from the function's own statements only: one
            // nested in a block leaves the rebinding skipped as local.
            Stmt::FunctionDef(stmt) => {
                let global = stmt.body.iter().any(|stmt| {
                    matches!(stmt, Stmt::Global(global)
                        if global.names.iter().any(|name| name.as_str() == "__all__"))
                });
                let scope = if global { Scope::Opaque } else { Scope::Local };
                self.body(&stmt.body, scope.max(Scope::Opaque), state);
            },
            Stmt::ClassDef(stmt) => self.body(&stmt.body, Scope::Local, state),
            _ => {},
        }
    }

    /// Read `body`, which may stop after any statement, into `state`, and
    /// return the join of every state it may stop in, the one before it
    /// included.
    fn cut_short(&mut self, body: &[Stmt], scope: Scope, state: &mut State, line: u32) -> State {
        let mut stopped = state.clone();
        for stmt in body {
            self.stmt(stmt, scope, state);
            stopped = stopped.join(state.clone(), line);
        }
        stopped
    }
}

/// Apply `update` read in `scope`; `Err` when it could not be read.
fn apply(update: AllUpdate, scope: Scope, state: &mut State) -> Result<(), ()> {
    let names = match (scope, update) {
        (Scope::Module, AllUpdate::Replace(Some(names))) => {
            *state = State {
                exports: Exports::Known(names),
                unbound: None,
            };
            return Ok(());
        },
        (Scope::Module, AllUpdate::Add(names)) => names,
        // An unread rebinding keeps the names read so far until gaps demote
        // findings (ADR 0005 step 3): rebuilding the list (`sorted(__all__)`)
        // keeps them, and dropping them would report them as CHK006.
        _ => None,
    };
    match (names, &mut state.exports) {
        (Some(names), Exports::Known(list) | Exports::Partial(list)) => {
            list.extend(names);
            Ok(())
        },
        // Already reported where `__all__` became unreadable.
        (Some(_), Exports::Unknown) => Ok(()),
        // Names added to a list that was never read would pass for the
        // whole `__all__` and hide the other public names.
        (Some(_), exports @ Exports::Absent) => {
            *exports = Exports::Unknown;
            Err(())
        },
        (None, exports) => {
            exports.lose();
            Err(())
        },
    }
}

/// One statement's effect on `__all__`; `None` in a payload means the names
/// could not be read.
enum AllUpdate {
    Replace(Option<Vec<String>>),
    Add(Option<Vec<String>>),
}

fn all_update(stmt: &Stmt) -> Option<AllUpdate> {
    match stmt {
        Stmt::Assign(assign) if assign.targets.iter().any(is_all) => Some(rebinding(&assign.value)),
        // `__all__[:] = names`, `__all__[0] = name`.
        Stmt::Assign(assign) if assign.targets.iter().any(is_all_subscript) => {
            Some(AllUpdate::Add(None))
        },
        Stmt::AnnAssign(assign) if is_all(&assign.target) => assign.value.as_deref().map(rebinding),
        Stmt::AugAssign(assign) if is_all(&assign.target) && assign.op == Operator::Add => {
            Some(AllUpdate::Add(literal_names(&assign.value)))
        },
        Stmt::ImportFrom(import) if import_binds_all(import) => Some(AllUpdate::Replace(None)),
        Stmt::Expr(stmt) => {
            let Expr::Call(call) = &*stmt.value else {
                return None;
            };
            let Expr::Attribute(method) = &*call.func else {
                return None;
            };
            if !is_all(&method.value) {
                return None;
            }
            let name = |value: &Expr| string_literal(value).map(|name| vec![name.to_owned()]);
            let names = match (method.attr.as_str(), &*call.arguments.args) {
                // Removes, reorders or only reads names. A list wider than the
                // runtime one only keeps names public, so these need not be
                // followed.
                (
                    "remove" | "pop" | "clear" | "sort" | "reverse" | "copy" | "index" | "count",
                    _,
                ) => {
                    return None;
                },
                _ if !call.arguments.keywords.is_empty() => None,
                ("extend", [names]) => literal_names(names),
                ("append", [value]) | ("insert", [_, value]) => name(value),
                _ => None,
            };
            Some(AllUpdate::Add(names))
        },
        _ => None,
    }
}

/// The update of `__all__ = value`: concatenating `__all__` with another
/// list adds that list (#876), anything else replaces it.
fn rebinding(value: &Expr) -> AllUpdate {
    if let Expr::BinOp(ast::ExprBinOp {
        left,
        op: Operator::Add,
        right,
        ..
    }) = value
    {
        match (is_all(left), is_all(right)) {
            (true, false) => return AllUpdate::Add(literal_names(right)),
            (false, true) => return AllUpdate::Add(literal_names(left)),
            _ => {},
        }
    }
    AllUpdate::Replace(literal_names(value))
}

/// Whether `stmt` binds the name `__all__`, which in a class or function
/// body makes a local name.
fn binds_all(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Assign(assign) => assign.targets.iter().any(is_all),
        Stmt::AnnAssign(assign) => is_all(&assign.target),
        Stmt::AugAssign(assign) => is_all(&assign.target),
        Stmt::ImportFrom(import) => import_binds_all(import),
        _ => false,
    }
}

fn import_binds_all(import: &ast::StmtImportFrom) -> bool {
    import
        .names
        .iter()
        .any(|alias| alias.asname.as_ref().unwrap_or(&alias.name).as_str() == "__all__")
}

fn is_all(target: &Expr) -> bool {
    matches!(target, Expr::Name(name) if name.id.as_str() == "__all__")
}

fn is_all_subscript(target: &Expr) -> bool {
    matches!(target, Expr::Subscript(subscript) if is_all(&subscript.value))
}

fn literal_names(expr: &Expr) -> Option<Vec<String>> {
    let elements = match expr {
        Expr::List(list) => &list.elts,
        Expr::Tuple(tuple) => &tuple.elts,
        _ => return None,
    };
    elements
        .iter()
        .map(|element| string_literal(element).map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(source: &str) -> (Exports, Vec<u32>) {
        let parsed = ruff_python_parser::parse_module(source).expect("parse");
        let mut diagnostics = Vec::new();
        let exports = extract_exports(parsed.suite(), &LineIndex::new(source), &mut diagnostics);
        let lines = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.line)
            .collect();
        (exports, lines)
    }

    fn known(names: &[&str]) -> Exports {
        Exports::Known(names.iter().map(|name| (*name).to_owned()).collect())
    }

    fn partial(names: &[&str]) -> Exports {
        Exports::Partial(names.iter().map(|name| (*name).to_owned()).collect())
    }

    #[test]
    fn extracts_all_list() {
        assert_eq!(
            read(r#"__all__ = ["foo", "bar"]"#),
            (known(&["foo", "bar"]), vec![])
        );
    }

    #[test]
    fn extracts_annotated_all() {
        assert_eq!(
            read("__all__: list[str] = [\"_helper\"]\n"),
            (known(&["_helper"]), vec![])
        );
    }

    #[test]
    fn a_module_without_all_is_absent() {
        assert_eq!(
            read("x = [\"a\"]\n__all__: list[str]\n"),
            (Exports::Absent, vec![])
        );
    }

    #[test]
    fn other_assignments_do_not_replace_exports() {
        let source = "__all__ = [\"foo\"]\nx = [\"bar\"]\ny: list[str] = [\"baz\"]\n\
                      z = f()\nw: list[str] = g()\n__all__.x = h()\n";
        assert_eq!(read(source), (known(&["foo"]), vec![]));
    }

    #[test]
    fn appends_literal_additions() {
        let source = "__all__ = [\"foo\"]\n__all__ += [\"bar\"]\n__all__ += (\"baz\",)\n\
                      __all__.extend([\"qux\"])\n__all__.extend((\"quux\",))\n\
                      __all__.append(\"corge\")\n";
        assert_eq!(
            read(source),
            (
                known(&["foo", "bar", "baz", "qux", "quux", "corge"]),
                vec![]
            )
        );
    }

    /// Conditional exports of optional features (mlflow, django gis) add to
    /// the list from `if` / `try` / `with` blocks.
    #[test]
    fn appends_from_blocks() {
        let source = "__all__ = [\"foo\"]\nif HAS_X:\n    __all__ += [\"a\"]\nelif HAS_Y:\n    \
                      __all__.append(\"b\")\ntry:\n    import x\nexcept ImportError:\n    \
                      __all__.extend([\"c\"])\nelse:\n    with suppress(Exception):\n        \
                      __all__.append(\"d\")\nfinally:\n    __all__ += (\"e\",)\n";
        assert_eq!(
            read(source),
            (known(&["foo", "a", "b", "c", "d", "e"]), vec![])
        );
    }

    /// An update that is not read keeps the names read so far and marks the
    /// list partial: a non-literal rebinding, a block assignment, a mutation
    /// in a loop, `match`, function or class body, a non-literal addition, or
    /// a method that may add.
    #[test]
    fn unread_updates_make_the_list_partial() {
        for source in [
            "if WIN:\n    __all__ = sorted(names)\n",
            "try:\n    import x\nexcept ImportError:\n    __all__ = __all__ + names\n",
            "for name in names:\n    __all__.append(name)\n",
            "while more():\n    __all__ += [\"x\"]\nelse:\n    pass\n",
            "match sys.platform:\n    case \"win32\":\n        __all__ += [\"win\"]\n",
            "def export(fn):\n    __all__.append(fn.__name__)\n    return fn\n",
            "if X:\n    def f():\n        __all__.append(\"f\")\n",
            "def f():\n    global __all__; __all__ = [\"g\"]\n",
            "class Exporter:\n    def __call__(self, fn):\n        __all__.append(fn.__name__)\n",
            "__all__ = sorted(__all__)\n",
            "__all__ = sorted(names)\n",
            "from .sub import __all__\n",
            "from .sub import names as __all__\n",
            "__all__ += sub.__all__\n",
            "__all__.extend(names)\n",
            "__all__.append(name)\n",
            "__all__.extend(\"ab\")\n",
            "__all__.append([\"x\"])\n",
            "__all__.append()\n",
            "__all__.extend([\"y\"], key=1)\n",
            "__all__.insert(0, name)\n",
            "__all__.insert(\"b\")\n",
            "__all__ = __all__ + names\n",
            "__all__ = other + __all__\n",
            "__all__ = __all__ + __all__\n",
            "__all__[:] = names\n",
        ] {
            let source = format!("__all__ = [\"foo\"]\n{source}");
            let (exports, lines) = read(&source);
            assert_eq!(exports, partial(&["foo"]), "{source}");
            let warned = lines
                .iter()
                .map(|line| source.lines().nth(*line as usize - 1));
            assert_eq!(
                warned.collect::<Vec<_>>(),
                [source.lines().skip(1).find(|line| line.contains("__all__"))],
                "{source}"
            );
        }
    }

    /// Concatenating a literal to `__all__` and inserting a literal name add
    /// to the list like `+=` and `.append` (#876).
    #[test]
    fn reads_concatenation_and_insert() {
        let source = "__all__ = [\"foo\"]\n__all__ = __all__ + [\"a\"]\n__all__ = (\"b\",) + __all__\n\
                      __all__: list[str] = __all__ + [\"c\"]\n__all__.insert(0, \"d\")\n\
                      if X:\n    __all__ = __all__ + [\"e\"]\n";
        assert_eq!(
            read(source),
            (known(&["foo", "a", "b", "c", "d", "e"]), vec![])
        );
    }

    /// A literal assignment in a block replaces the list on that path, and
    /// the paths join as the union of their names: an export that only an
    /// optional dependency adds is public too (#906).
    #[test]
    fn reads_assignments_in_blocks() {
        for (source, names) in [
            (
                "try:\n    from .app_auth import Auth\n    __all__ = [\"Client\", \"Auth\"]\n\
                 except ImportError:\n    __all__ = [\"Client\"]\n",
                &["Client", "Auth"][..],
            ),
            (
                "__all__ = [\"a\"]\ntry:\n    from ._speedups import fast\n    \
                 __all__ = [\"a\", \"fast\"]\nexcept ImportError:\n    pass\n",
                &["a", "fast"],
            ),
            (
                "__all__ = [\"old\"]\nif WIN:\n    __all__ = [\"a\"]\nelif MAC:\n    \
                 __all__ = (\"b\",)\nelse:\n    __all__: list[str] = [\"c\"]\n__all__.append(\"d\")\n",
                &["a", "b", "c", "d"],
            ),
            (
                "__all__ = [\"old\"]\nif X:\n    __all__ = [\"a\"]\n",
                &["old", "a"],
            ),
            (
                "__all__ = [\"old\"]\nwith suppress(ImportError):\n    __all__ = [\"a\"]\n",
                &["old", "a"],
            ),
            (
                "try:\n    import x\nexcept ImportError:\n    __all__ = [\"a\"]\nelse:\n    \
                 __all__ = [\"b\"]\n",
                &["a", "b"],
            ),
            (
                "if A:\n    if B:\n        __all__ = [\"x\"]\n    __all__ = [\"y\"]\nelse:\n    \
                 __all__ = [\"z\"]\n",
                &["y", "z"],
            ),
            (
                "try:\n    __all__ = [\"a\"]\nexcept ImportError:\n    pass\n__all__ = [\"b\"]\n",
                &["b"],
            ),
        ] {
            assert_eq!(read(source), (known(names), vec![]), "{source}");
        }
    }

    /// A block that assigns `__all__` on some paths only, with no list before
    /// it, may leave `__all__` unbound: the list is partial, warned at the
    /// block, since every public name may be exported instead.
    #[test]
    fn assigning_on_some_paths_only_is_partial() {
        for (source, names) in [
            ("if TYPE_CHECKING:\n    __all__: list[str] = []\n", &[][..]),
            (
                "try:\n    __all__ = [\"a\"]\nexcept ImportError:\n    pass\n",
                &["a"],
            ),
            (
                "try:\n    import x\nexcept ImportError:\n    __all__ = [\"a\"]\n__all__ += [\"b\"]\n",
                &["a", "b"],
            ),
        ] {
            assert_eq!(read(source), (partial(names), vec![1]), "{source}");
        }
    }

    /// One path the parser cannot read leaves nothing usable when no list
    /// was read before the block.
    #[test]
    fn an_unread_path_without_a_list_is_unknown() {
        let source = "if X:\n    __all__ = names()\nelse:\n    __all__ = [\"b\"]\n";
        assert_eq!(read(source), (Exports::Unknown, vec![2]));
    }

    /// Literal additions after an unread one still add to the partial list.
    #[test]
    fn partial_list_keeps_taking_literal_additions() {
        let source = "__all__ = [\"foo\"]\n__all__ += names\n__all__.append(\"bar\")\n";
        assert_eq!(read(source), (partial(&["foo", "bar"]), vec![2]));
    }

    /// A rebinding that cannot be read with no list before it is unknown; a
    /// later literal assignment reads again.
    #[test]
    fn unreadable_rebinding_without_a_list_is_unknown() {
        assert_eq!(
            read("__all__ = sorted(names)\n"),
            (Exports::Unknown, vec![1])
        );
        assert_eq!(
            read("__all__ = list(core.__all__)\n__all__ = [\"a\"]\n"),
            (known(&["a"]), vec![1])
        );
    }

    /// Names added to a list that could not be read would pass for the
    /// whole `__all__` and hide the other public names.
    #[test]
    fn additions_without_a_read_list_are_unknown() {
        for (source, lines) in [
            (
                "__all__ = list(core.__all__)\n__all__ += [\"extra\"]\n",
                vec![1],
            ),
            (
                "from .sub import __all__\n__all__.extend([\"extra\"])\n",
                vec![1],
            ),
            ("__all__.extend([\"extra\"])\n", vec![1]),
        ] {
            assert_eq!(read(source), (Exports::Unknown, lines), "{source}");
        }
    }

    /// Methods that remove, reorder or read names are not followed: a wider
    /// list only keeps names public.
    #[test]
    fn narrowing_and_reading_methods_are_ignored() {
        let source = "__all__ = [\"foo\"]\n__all__.sort()\n__all__.remove(\"foo\")\n\
                      __all__.pop()\n__all__.reverse()\n__all__.clear()\n__all__.copy()\n\
                      __all__.index(\"foo\")\n__all__.count(\"foo\")\n";
        assert_eq!(read(source), (known(&["foo"]), vec![]));
    }

    #[test]
    fn other_list_operations_do_not_touch_exports() {
        let source = "__all__ = [\"foo\"]\nx += [\"bar\"]\nx.extend([\"baz\"])\n\
                      x.__all__.append(\"qux\")\n__all__.append\nfrom .sub import other\n";
        assert_eq!(read(source), (known(&["foo"]), vec![]));
    }

    /// Binding `__all__` in a class or function body makes a local name.
    #[test]
    fn local_bindings_are_not_read() {
        let source = "__all__ = [\"foo\"]\nclass C:\n    __all__ = [\"attr\"]\n    \
                      __all__ += names\ndef test_all():\n    from pkg import __all__\n    \
                      __all__: list[str] = names\n    if X:\n        __all__ = []\n";
        assert_eq!(read(source), (known(&["foo"]), vec![]));
    }
}
