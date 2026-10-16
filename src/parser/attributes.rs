//! Attribute receiver resolution for `import module; module.name` symbol tracking.

use ruff_python_ast::Expr;

/// Append `a.b.c` to `out` as a dotted receiver name; `false`, with `out`
/// left partly written, for computed receivers.
pub(super) fn write_receiver(expr: &Expr, out: &mut String) -> bool {
    match expr {
        Expr::Name(name) => {
            out.push_str(name.id.as_str());
            true
        },
        Expr::Attribute(attribute) => {
            if !write_receiver(&attribute.value, out) {
                return false;
            }
            out.push('.');
            out.push_str(attribute.attr.as_str());
            true
        },
        _ => false,
    }
}

/// Whether `expr` is the dotted name `dotted`, compared without building it.
pub(super) fn is_receiver(expr: &Expr, dotted: &str) -> bool {
    match expr {
        Expr::Name(name) => name.id.as_str() == dotted,
        Expr::Attribute(attribute) => dotted
            .strip_suffix(attribute.attr.as_str())
            .and_then(|parent| parent.strip_suffix('.'))
            .is_some_and(|parent| is_receiver(&attribute.value, parent)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr(source: &str) -> Expr {
        ruff_python_parser::parse_expression(source)
            .expect("parse")
            .into_expr()
    }

    #[test]
    fn writes_dotted_receivers_only() {
        let mut out = String::new();
        assert!(write_receiver(&expr("a.b.c"), &mut out));
        assert_eq!(out, "a.b.c");
        assert!(!write_receiver(&expr("a().b"), &mut String::new()));
    }

    #[test]
    fn compares_dotted_receivers_segment_by_segment() {
        assert!(is_receiver(&expr("sys.path"), "sys.path"));
        assert!(is_receiver(&expr("site"), "site"));
        assert!(!is_receiver(&expr("sys.path"), "sys"));
        assert!(!is_receiver(&expr("os.sys.path"), "sys.path"));
        assert!(!is_receiver(&expr("sys_path"), "sys.path"));
        assert!(!is_receiver(&expr("x.sys.path"), "x.sys_path"));
        assert!(!is_receiver(&expr("f().path"), "f().path"));
    }
}
