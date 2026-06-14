//! `sys.platform` guard detection for conditional imports.

use rustpython_parser::ast::{CmpOp, Expr, Stmt};

/// Returns `true` when `stmt` is `if sys.platform …` (literal comparison only).
#[must_use]
pub fn is_platform_guard_if(stmt: &Stmt) -> bool {
    let Stmt::If(if_stmt) = stmt else {
        return false;
    };
    is_platform_guard_test(&if_stmt.test)
}

fn is_platform_guard_test(expr: &Expr) -> bool {
    let Expr::Compare(compare) = expr else {
        return false;
    };
    if compare.ops.len() != compare.comparators.len() {
        return false;
    }
    let platform_on_left = is_sys_platform_expr(&compare.left);
    let platform_on_right = compare.comparators.iter().any(is_sys_platform_expr);
    if !(platform_on_left || platform_on_right) {
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
    use rustpython_parser::Parse;
    use rustpython_parser::ast::Suite;

    use super::*;

    #[test]
    fn detects_sys_platform_equality() {
        let source = "import sys\nif sys.platform == 'win32':\n    pass\n";
        let stmts = Suite::parse(source, "<test>").expect("parse");
        assert!(stmts.iter().any(is_platform_guard_if));
    }

    #[test]
    fn detects_reversed_platform_comparison() {
        let source = "import sys\nif 'linux' == sys.platform:\n    pass\n";
        let stmts = Suite::parse(source, "<test>").expect("parse");
        assert!(stmts.iter().any(is_platform_guard_if));
    }

    #[test]
    fn rejects_non_platform_if() {
        let source = "if foo == 'bar':\n    pass\n";
        let stmts = Suite::parse(source, "<test>").expect("parse");
        assert!(!stmts.iter().any(is_platform_guard_if));
    }
}
