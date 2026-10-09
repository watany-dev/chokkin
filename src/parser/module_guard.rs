//! Branch conditions that tell an import is not a runtime requirement:
//! "already imported" checks and the `__main__` script block (#681).

use ruff_python_ast::{CmpOp, Expr};

/// The top-level module `test` proves is already imported when its body
/// runs: `"x" in sys.modules` or `sniffio.current_async_library() == "x"`.
#[must_use]
pub(super) fn imported_module_guard(test: &Expr) -> Option<String> {
    let Expr::Compare(compare) = test else {
        return None;
    };
    let ([op], [left, right]) = (&*compare.ops, &*compare.operands) else {
        return None;
    };
    let module = match op {
        CmpOp::In if is_dotted(right, "sys", "modules") => string_literal(left),
        CmpOp::Eq if is_async_library_call(left) => string_literal(right),
        CmpOp::Eq if is_async_library_call(right) => string_literal(left),
        _ => None,
    }?;
    module
        .split('.')
        .next()
        .filter(|root| !root.is_empty())
        .map(str::to_owned)
}

/// Returns `true` for `__name__ == "__main__"` (either operand order).
#[must_use]
pub(super) fn is_main_guard_test(test: &Expr) -> bool {
    let Expr::Compare(compare) = test else {
        return false;
    };
    let ([CmpOp::Eq], [left, right]) = (&*compare.ops, &*compare.operands) else {
        return false;
    };
    let is_name = |expr: &Expr| matches!(expr, Expr::Name(name) if name.id.as_str() == "__name__");
    let is_main = |expr: &Expr| string_literal(expr) == Some("__main__");
    (is_name(left) && is_main(right)) || (is_main(left) && is_name(right))
}

fn string_literal(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::StringLiteral(literal) => Some(literal.value.to_str()),
        _ => None,
    }
}

fn is_dotted(expr: &Expr, receiver: &str, attr: &str) -> bool {
    matches!(
        expr,
        Expr::Attribute(attribute)
            if attribute.attr.as_str() == attr
                && matches!(&*attribute.value, Expr::Name(name) if name.id.as_str() == receiver)
    )
}

fn is_async_library_call(expr: &Expr) -> bool {
    matches!(expr, Expr::Call(call)
        if call.arguments.is_empty() && is_dotted(&call.func, "sniffio", "current_async_library"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(source: &str) -> Option<String> {
        let parsed = ruff_python_parser::parse_expression(source).expect("parse");
        imported_module_guard(parsed.expr())
    }

    fn is_main(source: &str) -> bool {
        let parsed = ruff_python_parser::parse_expression(source).expect("parse");
        is_main_guard_test(parsed.expr())
    }

    #[test]
    fn detects_sys_modules_membership() {
        assert_eq!(
            guard("'pyspark.sql' in sys.modules").as_deref(),
            Some("pyspark")
        );
    }

    #[test]
    fn detects_async_library_comparison() {
        for source in [
            "sniffio.current_async_library() == 'trio'",
            "'trio' == sniffio.current_async_library()",
        ] {
            assert_eq!(guard(source).as_deref(), Some("trio"), "{source}");
        }
    }

    #[test]
    fn rejects_other_conditions() {
        for source in [
            "'x' not in sys.modules",
            "'x' in sys.modules and y",
            "'x' in modules",
            "sniffio.current_async_library() != 'trio'",
            "other() == 'trio'",
            "'trio' == other()",
            "name in sys.modules",
        ] {
            assert_eq!(guard(source), None, "{source}");
        }
    }

    #[test]
    fn detects_main_guard() {
        assert!(is_main("__name__ == '__main__'"));
        assert!(is_main("'__main__' == __name__"));
        assert!(!is_main("__name__ != '__main__'"));
        assert!(!is_main("__name__ == 'pkg'"));
        assert!(!is_main("'pkg' == __name__"));
    }
}
