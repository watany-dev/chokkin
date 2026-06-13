//! `TYPE_CHECKING` block detection helpers.

use rustpython_parser::ast::{Expr, Stmt};

/// Returns `true` when `stmt` is `if TYPE_CHECKING:` (or `if typing.TYPE_CHECKING:`).
#[must_use]
pub fn is_type_checking_if(stmt: &Stmt) -> bool {
    let Stmt::If(if_stmt) = stmt else {
        return false;
    };
    is_type_checking_test(&if_stmt.test)
}

fn is_type_checking_test(expr: &Expr) -> bool {
    match expr {
        Expr::Name(name) => name.id.as_str() == "TYPE_CHECKING",
        Expr::Attribute(attribute) => {
            attribute.attr.as_str() == "TYPE_CHECKING"
                && matches!(&*attribute.value, Expr::Name(name) if name.id.as_str() == "typing")
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
    fn detects_type_checking_if() {
        let source = "from typing import TYPE_CHECKING\nif TYPE_CHECKING:\n    pass\n";
        let stmts = Suite::parse(source, "<test>").expect("parse");
        assert!(stmts.iter().any(is_type_checking_if));
    }
}
