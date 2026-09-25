//! Decorator name normalization.
//!
//! Every statically named decorator is normalized here; which names matter is
//! decided by each consumer (`rules::symbols::external` for CHK006, the Flask
//! and Celery plugins for module references), so the parser holds no list.

use rustpython_parser::ast::Expr;

/// Normalize a decorator expression to a dotted name (`app.route`,
/// `functools.lru_cache`), or `None` when it has no static name.
#[must_use]
pub fn normalize_decorator(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(name) => Some(name.id.to_string()),
        Expr::Attribute(attribute) => {
            let parent = normalize_decorator(&attribute.value)?;
            Some(format!("{}.{}", parent, attribute.attr))
        },
        Expr::Call(call) => normalize_decorator(&call.func),
        // `@apps[0].route("/")`: the index is not static, but the suffix is.
        Expr::Subscript(subscript) => Some(format!("{}[]", normalize_decorator(&subscript.value)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rustpython_parser::Parse;
    use rustpython_parser::ast::{Stmt, Suite};

    use super::*;

    fn first_decorator(source: &str) -> Option<String> {
        let stmts = Suite::parse(source, "<test>").expect("parse");
        let Stmt::FunctionDef(function) = &stmts[0] else {
            panic!("expected function");
        };
        normalize_decorator(&function.decorator_list[0])
    }

    #[test]
    fn normalizes_pytest_fixture() {
        let normalized = first_decorator("@pytest.fixture\ndef sample():\n    pass\n");
        assert_eq!(normalized.as_deref(), Some("pytest.fixture"));
    }

    #[test]
    fn normalizes_subscript_receiver() {
        let normalized = first_decorator("@apps[0].route(\"/\")\ndef index():\n    pass\n");
        assert_eq!(normalized.as_deref(), Some("apps[].route"));
    }

    #[test]
    fn normalizes_decorators_outside_any_list() {
        let normalized =
            first_decorator("@functools.lru_cache(maxsize=1)\ndef cached():\n    pass\n");
        assert_eq!(normalized.as_deref(), Some("functools.lru_cache"));
    }
}
