//! `__all__` export list extraction.

use ruff_python_ast::{ExceptHandler, Expr, Operator, Stmt};
use ruff_text_size::Ranged;

use super::lines::LineIndex;
use super::module_guard::string_literal;
use super::types::ParseDiagnostic;
use super::types::ParseSeverity;

/// Extract `__all__` names and emit warnings for unsupported forms.
///
/// A literal assignment replaces the list; a later literal `+=`, `.extend`
/// or `.append` adds to it, also from inside `if` / `try` / `with` blocks
/// (#567).
pub(super) fn extract_exports(
    stmts: &[Stmt],
    lines: &LineIndex,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Vec<String> {
    let mut exports = None;
    read_body(stmts, true, lines, &mut exports, diagnostics);
    exports.unwrap_or_default()
}

/// Apply the `__all__` updates in `stmts` in source order. Inside a block
/// (`top_level` false) only additions count: which branch runs is unknown,
/// and adding the names of every branch keeps each optional export public.
/// Until a top-level literal assignment there is no list (`None`) to add
/// to: the added names alone would pass for the whole `__all__`.
fn read_body(
    stmts: &[Stmt],
    top_level: bool,
    lines: &LineIndex,
    exports: &mut Option<Vec<String>>,
    diagnostics: &mut Vec<ParseDiagnostic>,
) {
    for stmt in stmts {
        {
            let mut read_block =
                |body: &[Stmt]| read_body(body, false, lines, exports, diagnostics);
            match stmt {
                Stmt::If(stmt) => {
                    read_block(&stmt.body);
                    for clause in &stmt.elif_else_clauses {
                        read_block(&clause.body);
                    }
                },
                Stmt::Try(stmt) => {
                    read_block(&stmt.body);
                    for ExceptHandler::ExceptHandler(handler) in &stmt.handlers {
                        read_block(&handler.body);
                    }
                    read_block(&stmt.orelse);
                    read_block(&stmt.finalbody);
                },
                Stmt::With(stmt) => read_block(&stmt.body),
                _ => {},
            }
        }
        let Some(update) = all_update(stmt) else {
            continue;
        };
        let (names, replace) = match update {
            AllUpdate::Replace(_) if !top_level => continue,
            AllUpdate::Replace(value) => (literal_names(value), true),
            AllUpdate::Extend(value) => (literal_names(value), false),
            AllUpdate::Append(value) => (
                string_literal(value).map(|name| vec![name.to_owned()]),
                false,
            ),
            AllUpdate::Unreadable => (None, false),
        };
        match names {
            Some(names) if replace => *exports = Some(names),
            Some(names) => {
                if let Some(exports) = exports {
                    exports.extend(names);
                }
            },
            None => diagnostics.push(ParseDiagnostic {
                line: lines.line(stmt.start()),
                message: "unsupported `__all__` assignment form".to_owned(),
                severity: ParseSeverity::Warning,
            }),
        }
    }
}

/// How a statement changes `__all__`, with the expression that
/// carries the names.
enum AllUpdate<'a> {
    /// `__all__ = …` or `__all__: T = …`.
    Replace(&'a Expr),
    /// `__all__ += …` or `__all__.extend(…)`.
    Extend(&'a Expr),
    /// `__all__.append(…)`.
    Append(&'a Expr),
    /// `__all__.extend(…)` or `.append(…)` without exactly one positional
    /// argument.
    Unreadable,
}

fn all_update(stmt: &Stmt) -> Option<AllUpdate<'_>> {
    match stmt {
        Stmt::Assign(assign) if assign.targets.iter().any(is_all) => {
            Some(AllUpdate::Replace(&assign.value))
        },
        Stmt::AnnAssign(assign) if is_all(&assign.target) => {
            assign.value.as_deref().map(AllUpdate::Replace)
        },
        Stmt::AugAssign(assign) if is_all(&assign.target) && assign.op == Operator::Add => {
            Some(AllUpdate::Extend(&assign.value))
        },
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
            let update = match method.attr.as_str() {
                "extend" => AllUpdate::Extend,
                "append" => AllUpdate::Append,
                _ => return None,
            };
            match (&*call.arguments.args, &*call.arguments.keywords) {
                ([argument], []) => Some(update(argument)),
                _ => Some(AllUpdate::Unreadable),
            }
        },
        _ => None,
    }
}

fn is_all(target: &Expr) -> bool {
    matches!(target, Expr::Name(name) if name.id.as_str() == "__all__")
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

    #[test]
    fn extracts_all_list() {
        let source = r#"__all__ = ["foo", "bar"]"#;
        let parsed = ruff_python_parser::parse_module(source).expect("parse");
        let mut diagnostics = Vec::new();
        let exports = extract_exports(parsed.suite(), &LineIndex::new(source), &mut diagnostics);
        assert_eq!(exports, vec!["foo".to_owned(), "bar".to_owned()]);
        assert_eq!(diagnostics, []);
    }

    #[test]
    fn extracts_annotated_all() {
        let source = "__all__: list[str] = [\"_helper\"]\n";
        let parsed = ruff_python_parser::parse_module(source).expect("parse");
        let mut diagnostics = Vec::new();
        let exports = extract_exports(parsed.suite(), &LineIndex::new(source), &mut diagnostics);
        assert_eq!(exports, vec!["_helper".to_owned()]);
        assert_eq!(diagnostics, []);
    }

    #[test]
    fn other_assignments_do_not_replace_exports() {
        let source = "__all__ = [\"foo\"]\nx = [\"bar\"]\ny: list[str] = [\"baz\"]\n\
                      z = f()\nw: list[str] = g()\n__all__.x = h()\n";
        let parsed = ruff_python_parser::parse_module(source).expect("parse");
        let mut diagnostics = Vec::new();
        let exports = extract_exports(parsed.suite(), &LineIndex::new(source), &mut diagnostics);
        assert_eq!(exports, vec!["foo".to_owned()]);
        assert_eq!(diagnostics, []);
    }

    #[test]
    fn appends_literal_additions() {
        let source = "__all__ = [\"foo\"]\n__all__ += [\"bar\"]\n__all__ += (\"baz\",)\n\
                      __all__.extend([\"qux\"])\n__all__.extend((\"quux\",))\n\
                      __all__.append(\"corge\")\n";
        let parsed = ruff_python_parser::parse_module(source).expect("parse");
        let mut diagnostics = Vec::new();
        let exports = extract_exports(parsed.suite(), &LineIndex::new(source), &mut diagnostics);
        assert_eq!(exports, ["foo", "bar", "baz", "qux", "quux", "corge"]);
        assert_eq!(diagnostics, []);
    }

    /// Conditional exports of optional features (mlflow, django gis) add to
    /// the list from `if` / `try` / `with` blocks; an assignment there does
    /// not replace it, and function bodies are not read.
    #[test]
    fn appends_from_blocks_but_not_assignments_or_functions() {
        let source = "__all__ = [\"foo\"]\nif HAS_X:\n    __all__ += [\"a\"]\nelif HAS_Y:\n    \
                      __all__.append(\"b\")\nelse:\n    __all__ = [\"replaced\"]\ntry:\n    \
                      import x\nexcept ImportError:\n    __all__.extend([\"c\"])\nelse:\n    \
                      with suppress(Exception):\n        __all__.append(\"d\")\nfinally:\n    \
                      __all__ += (\"e\",)\ndef f():\n    __all__.append(\"inner\")\n\
                      class C:\n    __all__ += [\"attr\"]\nif HAS_Z:\n    __all__ += names\n";
        let parsed = ruff_python_parser::parse_module(source).expect("parse");
        let mut diagnostics = Vec::new();
        let exports = extract_exports(parsed.suite(), &LineIndex::new(source), &mut diagnostics);
        assert_eq!(exports, ["foo", "a", "b", "c", "d", "e"]);
        let lines: Vec<_> = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.line)
            .collect();
        assert_eq!(lines, [22]);
    }

    #[test]
    fn non_literal_additions_warn_and_keep_the_list() {
        let source = "__all__ = [\"foo\"]\n__all__ += sub.__all__\n__all__.extend(names)\n\
                      __all__.append(name)\n__all__.extend(\"ab\")\n__all__.append([\"x\"])\n\
                      __all__.append()\n__all__.extend([\"y\"], key=1)\n";
        let parsed = ruff_python_parser::parse_module(source).expect("parse");
        let mut diagnostics = Vec::new();
        let exports = extract_exports(parsed.suite(), &LineIndex::new(source), &mut diagnostics);
        assert_eq!(exports, ["foo"]);
        let lines: Vec<_> = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.line)
            .collect();
        assert_eq!(lines, [2, 3, 4, 5, 6, 7, 8]);
    }

    /// Names added to a list that could not be read would pass for the
    /// whole `__all__` and hide the other public names.
    #[test]
    fn additions_without_a_literal_list_are_dropped() {
        for source in [
            "__all__ = list(core.__all__)\n__all__ += [\"extra\"]\n",
            "if WIN:\n    __all__ = [\"a\"]\nelse:\n    __all__ = [\"b\"]\n__all__.append(\"d\")\n",
            "from .sub import __all__\n__all__.extend([\"extra\"])\n",
        ] {
            let parsed = ruff_python_parser::parse_module(source).expect("parse");
            let mut diagnostics = Vec::new();
            let exports =
                extract_exports(parsed.suite(), &LineIndex::new(source), &mut diagnostics);
            assert_eq!(exports, Vec::<String>::new(), "{source}");
        }
    }

    #[test]
    fn other_list_operations_do_not_touch_exports() {
        let source = "__all__ = [\"foo\"]\nx += [\"bar\"]\nx.extend([\"baz\"])\n\
                      x.__all__.append(\"qux\")\n__all__.sort()\n__all__.append\n";
        let parsed = ruff_python_parser::parse_module(source).expect("parse");
        let mut diagnostics = Vec::new();
        let exports = extract_exports(parsed.suite(), &LineIndex::new(source), &mut diagnostics);
        assert_eq!(exports, ["foo"]);
        assert_eq!(diagnostics, []);
    }
}
