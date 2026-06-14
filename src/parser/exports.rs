//! `__all__` export list extraction.

use rustpython_parser::ast::Ranged;
use rustpython_parser::ast::{Expr, Stmt};
use rustpython_parser::source_code::RandomLocator;

use super::types::ParseDiagnostic;
use super::types::ParseSeverity;

/// Extract `__all__` names and emit warnings for unsupported forms.
pub fn extract_exports(
    stmts: &[Stmt],
    locator: &mut RandomLocator<'_>,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Vec<String> {
    let mut exports = Vec::new();
    for stmt in stmts {
        if let Stmt::Assign(assign) = stmt
            && is_all_target(&assign.targets)
        {
            match literal_names(&assign.value) {
                Some(names) => exports = names,
                None => diagnostics.push(ParseDiagnostic {
                    line: locator.locate(assign.start()).row.get(),
                    message: "unsupported `__all__` assignment form".to_owned(),
                    severity: ParseSeverity::Warning,
                }),
            }
        }
    }
    exports
}

fn is_all_target(targets: &[Expr]) -> bool {
    targets
        .iter()
        .any(|target| matches!(target, Expr::Name(name) if name.id.as_str() == "__all__"))
}

fn literal_names(expr: &Expr) -> Option<Vec<String>> {
    let elements = match expr {
        Expr::List(list) => &list.elts,
        Expr::Tuple(tuple) => &tuple.elts,
        _ => return None,
    };
    let mut names = Vec::new();
    for element in elements {
        let Expr::Constant(constant) = element else {
            return None;
        };
        let rustpython_parser::ast::Constant::Str(value) = &constant.value else {
            return None;
        };
        names.push(value.clone());
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use rustpython_parser::Parse;
    use rustpython_parser::ast::Suite;

    use super::*;

    #[test]
    fn extracts_all_list() {
        let source = r#"__all__ = ["foo", "bar"]"#;
        let stmts = Suite::parse(source, "<test>").expect("parse");
        let mut diagnostics = Vec::new();
        let mut locator = RandomLocator::new(source);
        let exports = extract_exports(&stmts, &mut locator, &mut diagnostics);
        assert_eq!(exports, vec!["foo".to_owned(), "bar".to_owned()]);
        assert!(diagnostics.is_empty());
    }
}
