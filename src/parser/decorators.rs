//! Decorator name normalization for externally-used symbol hints.

use rustpython_parser::ast::Expr;

/// Known decorator prefixes (v0.1 exact-match list).
const KNOWN_DECORATOR_SUFFIXES: &[&str] = &[
    "get",
    "post",
    "put",
    "delete",
    "patch",
    "route",
    "fixture",
    "shared_task",
    "task",
    "command",
];

/// Normalize a decorator expression to a dotted name when recognized.
#[must_use]
pub fn normalize_decorator(expr: &Expr) -> Option<String> {
    let name = expr_to_dotted(expr)?;
    if is_known_decorator(&name) {
        Some(name)
    } else {
        None
    }
}

fn is_known_decorator(name: &str) -> bool {
    if name.starts_with("pytest.mark.") {
        return true;
    }
    if let Some((_, suffix)) = name.rsplit_once('.') {
        return KNOWN_DECORATOR_SUFFIXES.contains(&suffix);
    }
    KNOWN_DECORATOR_SUFFIXES.contains(&name)
}

fn expr_to_dotted(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(name) => Some(name.id.to_string()),
        Expr::Attribute(attribute) => {
            let parent = expr_to_dotted(&attribute.value)?;
            Some(format!("{}.{}", parent, attribute.attr))
        },
        Expr::Call(call) => expr_to_dotted(&call.func),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rustpython_parser::Parse;
    use rustpython_parser::ast::{Stmt, Suite};

    use super::*;

    #[test]
    fn normalizes_pytest_fixture() {
        let source = "@pytest.fixture\ndef sample():\n    pass\n";
        let stmts = Suite::parse(source, "<test>").expect("parse");
        let Stmt::FunctionDef(function) = &stmts[0] else {
            panic!("expected function");
        };
        let normalized = normalize_decorator(&function.decorator_list[0]).expect("decorator");
        assert_eq!(normalized, "pytest.fixture");
    }
}
