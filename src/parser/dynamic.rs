//! `importlib.import_module` and `__import__` literal recognition.

use rustpython_parser::ast::Expr;

/// Module name when `func(args…)` is a dynamic import with a literal argument.
pub fn extract_literal_module_call(func: &Expr, args: &[Expr]) -> Option<String> {
    if !is_import_module_call(func) {
        return None;
    }
    string_literal(args.first()?)
}

/// Whether `func` names `importlib.import_module` or `__import__`.
pub fn is_import_module_call(func: &Expr) -> bool {
    match func {
        Expr::Attribute(attribute) => {
            attribute.attr.as_str() == "import_module"
                && matches!(
                    &*attribute.value,
                    Expr::Name(name) if name.id.as_str() == "importlib"
                )
        },
        Expr::Name(name) => name.id.as_str() == "__import__",
        _ => false,
    }
}

fn string_literal(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Constant(constant) => match &constant.value {
            rustpython_parser::ast::Constant::Str(value) => Some(value.clone()),
            _ => None,
        },
        _ => None,
    }
}
