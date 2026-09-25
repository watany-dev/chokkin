//! `TYPE_CHECKING` block detection helpers.

use std::collections::HashSet;

use rustpython_parser::ast::{Expr, Stmt};

/// Returns `true` when `stmt` is `if TYPE_CHECKING:` (or `if typing.TYPE_CHECKING:`).
#[must_use]
pub fn is_type_checking_if(
    stmt: &Stmt,
    typing_aliases: &HashSet<String>,
    type_checking_names: &HashSet<String>,
) -> bool {
    let Stmt::If(if_stmt) = stmt else {
        return false;
    };
    is_type_checking_test(&if_stmt.test, typing_aliases, type_checking_names)
}

fn is_type_checking_test(
    expr: &Expr,
    typing_aliases: &HashSet<String>,
    type_checking_names: &HashSet<String>,
) -> bool {
    match expr {
        Expr::Name(name) => type_checking_names.contains(name.id.as_str()),
        Expr::Attribute(attribute) => {
            attribute.attr.as_str() == "TYPE_CHECKING"
                && matches!(
                    &*attribute.value,
                    Expr::Name(name) if typing_aliases.contains(name.id.as_str())
                )
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
        let typing_aliases = HashSet::from(["typing".to_owned()]);
        let type_checking_names = HashSet::from(["TYPE_CHECKING".to_owned()]);
        assert!(stmts.iter().any(|stmt| is_type_checking_if(
            stmt,
            &typing_aliases,
            &type_checking_names
        )));
    }

    #[test]
    fn detects_type_checking_aliases() {
        let source = "import typing as t\nif t.TYPE_CHECKING:\n    pass\n";
        let stmts = Suite::parse(source, "<test>").expect("parse");
        let typing_aliases = HashSet::from(["typing".to_owned(), "t".to_owned()]);
        let type_checking_names = HashSet::from(["TYPE_CHECKING".to_owned()]);
        assert!(stmts.iter().any(|stmt| is_type_checking_if(
            stmt,
            &typing_aliases,
            &type_checking_names
        )));
    }
}
