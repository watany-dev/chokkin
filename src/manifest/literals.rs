//! Static extraction of Python string and string-list literals from the AST.

use ruff_python_ast::{Expr, Stmt, Suite};
use ruff_python_parser::Parsed;

/// Result of reading a Python list literal for string elements.
#[derive(Debug)]
pub struct LiteralScan {
    /// String elements in source order.
    pub values: Vec<String>,
    /// `false` when the list also holds non-string elements.
    pub complete: bool,
}

/// Parse Python source without executing it; `None` on a syntax error.
pub fn parse_module(contents: &str) -> Option<Suite> {
    ruff_python_parser::parse_module(contents)
        .ok()
        .map(Parsed::into_suite)
}

/// Value of the first top-level `name = ...` assignment.
pub fn assigned_value<'a>(stmts: &'a [Stmt], name: &str) -> Option<&'a Expr> {
    stmts.iter().find_map(|stmt| {
        let Stmt::Assign(assign) = stmt else {
            return None;
        };
        let assigns_name = assign
            .targets
            .iter()
            .any(|target| matches!(target, Expr::Name(target) if target.id.as_str() == name));
        assigns_name.then_some(&*assign.value)
    })
}

/// The string when `expr` is a string literal.
pub fn string_value(expr: &Expr) -> Option<String> {
    if let Expr::StringLiteral(literal) = expr {
        Some(literal.value.to_str().to_owned())
    } else {
        None
    }
}

/// The string elements when `expr` is a list literal.
pub fn string_list(expr: &Expr) -> Option<LiteralScan> {
    let Expr::List(list) = expr else {
        return None;
    };
    let values: Vec<String> = list.elts.iter().filter_map(string_value).collect();
    Some(LiteralScan {
        complete: values.len() == list.elts.len(),
        values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list_assignment(contents: &str, name: &str) -> Option<LiteralScan> {
        let stmts = parse_module(contents).expect("parse");
        assigned_value(&stmts, name).and_then(string_list)
    }

    #[test]
    fn string_list_skips_inline_comments() {
        let scan = list_assignment("X = [\"a\", # comment\n \"b\"]\n", "X").expect("found");
        assert!(scan.complete);
        assert_eq!(scan.values, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn string_list_marks_non_string_elements_incomplete() {
        let scan = list_assignment("X = [\"a\", other, \"b\"]\n", "X").expect("found");
        assert!(!scan.complete);
        assert_eq!(scan.values, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn assigned_value_finds_lowercase_name() {
        let contents = r#"
extensions = [
    "sphinx.ext.autodoc",
    "myst_parser",
]
"#;
        let scan = list_assignment(contents, "extensions").expect("found");
        assert!(scan.complete);
        assert_eq!(
            scan.values,
            vec!["sphinx.ext.autodoc".to_owned(), "myst_parser".to_owned()]
        );
    }

    #[test]
    fn non_list_assignment_is_not_a_scan() {
        assert!(list_assignment("X = build()\n", "X").is_none());
    }

    #[test]
    fn string_value_reads_root_urlconf() {
        let stmts = parse_module(r#"ROOT_URLCONF = "myproject.urls""#).expect("parse");
        assert_eq!(
            assigned_value(&stmts, "ROOT_URLCONF")
                .and_then(string_value)
                .as_deref(),
            Some("myproject.urls")
        );
    }

    #[test]
    fn syntax_error_is_none() {
        assert!(parse_module("X = [\"a\",").is_none());
    }
}
