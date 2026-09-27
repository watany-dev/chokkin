//! Attribute receiver resolution for `import module; module.name` symbol tracking.

use ruff_python_ast::Expr;

/// Flatten `a.b.c` into a dotted receiver name, or `None` for computed receivers.
pub fn attribute_receiver(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(name) => Some(name.id.to_string()),
        Expr::Attribute(attribute) => {
            let parent = attribute_receiver(&attribute.value)?;
            Some(format!("{}.{}", parent, attribute.attr))
        },
        _ => None,
    }
}
