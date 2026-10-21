//! Static extraction of Python string and string-list literals from the AST.

use ruff_python_ast::{Expr, Stmt, Suite};
use ruff_python_parser::Parsed;
use ruff_text_size::{Ranged, TextSize};

/// Result of reading a Python list literal for string elements.
#[derive(Debug)]
pub(crate) struct LiteralScan {
    /// String elements in source order.
    pub values: Vec<String>,
    /// Source offset of each string in `values`, for its line.
    pub starts: Vec<TextSize>,
    /// `false` when the list also holds non-string elements.
    pub complete: bool,
}

/// Parse Python source without executing it; `None` on a syntax error or
/// nesting too deep to walk (#1045).
pub(crate) fn parse_module(contents: &str) -> Option<Suite> {
    crate::parser::parse_walkable_module(contents)
        .ok()
        .map(Parsed::into_suite)
}

/// Value of the first top-level `name = ...` assignment.
pub(crate) fn assigned_value<'a>(stmts: &'a [Stmt], name: &str) -> Option<&'a Expr> {
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
pub(crate) fn string_value(expr: &Expr) -> Option<String> {
    if let Expr::StringLiteral(literal) = expr {
        Some(literal.value.to_str().to_owned())
    } else {
        None
    }
}

/// The string elements when `expr` is a list or tuple literal (Django's
/// `INSTALLED_APPS = (...)`); any other expression is no scan.
pub(crate) fn string_list(expr: &Expr) -> Option<LiteralScan> {
    let elts = match expr {
        Expr::List(list) => &list.elts,
        Expr::Tuple(tuple) => &tuple.elts,
        _ => return None,
    };
    let (values, starts): (Vec<String>, Vec<TextSize>) = elts
        .iter()
        .filter_map(|elt| Some((string_value(elt)?, elt.start())))
        .unzip();
    Some(LiteralScan {
        complete: values.len() == elts.len(),
        values,
        starts,
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
        assert_eq!(scan.starts, [TextSize::new(5), TextSize::new(21)]);
    }

    #[test]
    fn string_list_marks_non_string_elements_incomplete() {
        let scan = list_assignment("X = [\"a\", other, \"b\"]\n", "X").expect("found");
        assert!(!scan.complete);
        assert_eq!(scan.values, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn string_list_reads_a_tuple() {
        let scan = list_assignment("X = (\"a\", \"b\")\n", "X").expect("found");
        assert!(scan.complete);
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

    #[test]
    fn nesting_too_deep_to_walk_is_none() {
        let deep = format!("X = {}1\n", "-".repeat(100_000));
        assert!(parse_module(&deep).is_none());
    }
}
