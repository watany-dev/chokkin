//! `sys.platform` guard detection for conditional imports.

use ruff_python_ast::{CmpOp, Expr};

/// Returns `true` when `expr` compares `sys.platform` (literal comparison only).
#[must_use]
pub fn is_platform_guard_test(expr: &Expr) -> bool {
    let Expr::Compare(compare) = expr else {
        return false;
    };
    if !compare.operands.iter().any(is_sys_platform_expr) {
        return false;
    }
    compare.ops.iter().all(|op| {
        matches!(
            op,
            CmpOp::Eq | CmpOp::NotEq | CmpOp::Lt | CmpOp::LtE | CmpOp::Gt | CmpOp::GtE
        )
    })
}

fn is_sys_platform_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Attribute(attribute) => {
            attribute.attr.as_str() == "platform"
                && matches!(&*attribute.value, Expr::Name(name) if name.id.as_str() == "sys")
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_platform_guard(source: &str) -> bool {
        let parsed = ruff_python_parser::parse_expression(source).expect("parse");
        is_platform_guard_test(parsed.expr())
    }

    #[test]
    fn detects_sys_platform_equality() {
        assert!(is_platform_guard("sys.platform == 'win32'"));
    }

    #[test]
    fn detects_reversed_platform_comparison() {
        assert!(is_platform_guard("'linux' == sys.platform"));
    }

    #[test]
    fn rejects_non_platform_if() {
        assert!(!is_platform_guard("foo == 'bar'"));
    }
}
