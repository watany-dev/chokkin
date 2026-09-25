//! `TYPE_CHECKING` block detection helpers.

use std::collections::HashSet;

use ruff_python_ast::Expr;

/// Returns `true` when `expr` is `TYPE_CHECKING` (or `typing.TYPE_CHECKING`), aliases included.
#[must_use]
pub fn is_type_checking_test(
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
    use super::*;

    fn is_type_checking(source: &str, typing_aliases: &[&str]) -> bool {
        let parsed = ruff_python_parser::parse_expression(source).expect("parse");
        let typing_aliases = typing_aliases.iter().map(ToString::to_string).collect();
        let type_checking_names = HashSet::from(["TYPE_CHECKING".to_owned()]);
        is_type_checking_test(parsed.expr(), &typing_aliases, &type_checking_names)
    }

    #[test]
    fn detects_type_checking_name() {
        assert!(is_type_checking("TYPE_CHECKING", &["typing"]));
    }

    #[test]
    fn detects_type_checking_aliases() {
        assert!(is_type_checking("t.TYPE_CHECKING", &["typing", "t"]));
    }
}
