//! `__all__` export list extraction.

use ruff_python_ast::{self as ast, ExceptHandler, Expr, Operator, Stmt};
use ruff_text_size::Ranged;

use super::lines::LineIndex;
use super::module_guard::string_literal;
use super::types::{Exports, ParseDiagnostic, ParseSeverity};

/// Read `__all__` and warn about each update that could not be read.
///
/// A literal assignment replaces the list; a later literal `+=`, `.extend`,
/// `.append`, `.insert` or `__all__ + [...]` adds to it, also from inside `if` / `try` / `with` blocks
/// (#567). Any other update leaves the result [`Exports::Partial`] or
/// [`Exports::Unknown`] (ADR 0005).
pub(super) fn extract_exports(
    stmts: &[Stmt],
    lines: &LineIndex,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Exports {
    let mut exports = Exports::Absent;
    read_body(stmts, Scope::TopLevel, lines, &mut exports, diagnostics);
    exports
}

/// Where a statement runs, which decides what its update can tell. Ordered
/// from most to least readable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    /// Module level: runs once, in order.
    TopLevel,
    /// `if` / `try` / `with` blocks: which branch runs is unknown, and adding
    /// the names of every branch keeps each optional export public, so only
    /// additions are read. An assignment there may or may not replace the
    /// list.
    Branch,
    /// Loop and `match` bodies and function bodies: they run any number of
    /// times, or only when called, so no update there is read (#875).
    Opaque,
    /// A class body, or a function body without `global __all__`: binding
    /// `__all__` there makes a local name, so only mutations of the module
    /// list count, and like [`Self::Opaque`] none of them is read.
    Local,
}

fn read_body(
    stmts: &[Stmt],
    scope: Scope,
    lines: &LineIndex,
    exports: &mut Exports,
    diagnostics: &mut Vec<ParseDiagnostic>,
) {
    for stmt in stmts {
        read_nested(stmt, scope, lines, exports, diagnostics);
        if scope == Scope::Local && binds_all(stmt) {
            continue;
        }
        let Some(update) = all_update(stmt) else {
            continue;
        };
        if apply(update, scope, exports).is_err() {
            diagnostics.push(ParseDiagnostic {
                line: lines.line(stmt.start()),
                message: "unsupported `__all__` assignment form".to_owned(),
                severity: ParseSeverity::Warning,
            });
        }
    }
}

fn read_nested(
    stmt: &Stmt,
    scope: Scope,
    lines: &LineIndex,
    exports: &mut Exports,
    diagnostics: &mut Vec<ParseDiagnostic>,
) {
    let branch = scope.max(Scope::Branch);
    let opaque = scope.max(Scope::Opaque);
    let mut read = |body: &[Stmt], scope| read_body(body, scope, lines, exports, diagnostics);
    match stmt {
        Stmt::If(stmt) => {
            read(&stmt.body, branch);
            for clause in &stmt.elif_else_clauses {
                read(&clause.body, branch);
            }
        },
        Stmt::Try(stmt) => {
            read(&stmt.body, branch);
            for ExceptHandler::ExceptHandler(handler) in &stmt.handlers {
                read(&handler.body, branch);
            }
            read(&stmt.orelse, branch);
            read(&stmt.finalbody, branch);
        },
        Stmt::With(stmt) => read(&stmt.body, branch),
        Stmt::For(ast::StmtFor { body, orelse, .. })
        | Stmt::While(ast::StmtWhile { body, orelse, .. }) => {
            read(body, opaque);
            read(orelse, opaque);
        },
        Stmt::Match(stmt) => {
            for case in &stmt.cases {
                read(&case.body, opaque);
            }
        },
        // An `export` decorator appending `fn.__name__` is the usual case.
        // `global` is read from the function's own statements only: one
        // nested in a block leaves the rebinding skipped as local.
        Stmt::FunctionDef(stmt) => {
            let global = stmt.body.iter().any(|stmt| {
                matches!(stmt, Stmt::Global(global)
                    if global.names.iter().any(|name| name.as_str() == "__all__"))
            });
            read(
                &stmt.body,
                if global { Scope::Opaque } else { Scope::Local },
            );
        },
        Stmt::ClassDef(stmt) => read(&stmt.body, Scope::Local),
        _ => {},
    }
}

/// Apply `update` read in `scope`; `Err` when it could not be read.
fn apply(update: AllUpdate, scope: Scope, exports: &mut Exports) -> Result<(), ()> {
    let names = match (scope, update) {
        (Scope::TopLevel, AllUpdate::Replace(Some(names))) => {
            *exports = Exports::Known(names);
            return Ok(());
        },
        (Scope::TopLevel | Scope::Branch, AllUpdate::Add(names)) => names,
        // An unread rebinding keeps the names read so far until gaps demote
        // findings (ADR 0005 step 3): rebuilding the list (`sorted(__all__)`)
        // keeps them, and dropping them would report them as CHK006.
        _ => None,
    };
    match (names, exports) {
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
            "if WIN:\n    __all__ = [\"replaced\"]\n",
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
                "if WIN:\n    __all__ = [\"a\"]\nelse:\n    __all__ = [\"b\"]\n__all__.append(\"d\")\n",
                vec![2, 4],
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
