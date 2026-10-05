//! `__all__` export list extraction.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use super::lines::LineIndex;
use super::types::ParseDiagnostic;
use super::types::ParseSeverity;

/// Extract `__all__` names and emit warnings for unsupported forms.
pub fn extract_exports(
    stmts: &[Stmt],
    lines: &LineIndex,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Vec<String> {
    let mut exports = Vec::new();
    for stmt in stmts {
        let value = match stmt {
            Stmt::Assign(assign) if assign.targets.iter().any(is_all) => &assign.value,
            Stmt::AnnAssign(assign) if is_all(&assign.target) => match &assign.value {
                Some(value) => value,
                None => continue,
            },
            _ => continue,
        };
        match literal_names(value) {
            Some(names) => exports = names,
            None => diagnostics.push(ParseDiagnostic {
                line: lines.line(stmt.start()),
                message: "unsupported `__all__` assignment form".to_owned(),
                severity: ParseSeverity::Warning,
            }),
        }
    }
    exports
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
        .map(|element| match element {
            Expr::StringLiteral(literal) => Some(literal.value.to_str().to_owned()),
            _ => None,
        })
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
}
