//! `importlib.import_module` and `__import__` literal extraction.

use rustpython_parser::ast::Ranged;
use rustpython_parser::ast::{Expr, Stmt};
use rustpython_parser::source_code::RandomLocator;

use super::types::DynamicImport;

/// Scan statements for literal dynamic import calls.
pub fn collect_dynamic_imports(
    stmts: &[Stmt],
    locator: &mut RandomLocator<'_>,
    out: &mut Vec<DynamicImport>,
    opaque: &mut bool,
) {
    for stmt in stmts {
        collect_from_stmt(stmt, locator, out, opaque);
    }
}

#[allow(clippy::too_many_lines)]
fn collect_from_stmt(
    stmt: &Stmt,
    locator: &mut RandomLocator<'_>,
    out: &mut Vec<DynamicImport>,
    opaque: &mut bool,
) {
    match stmt {
        Stmt::FunctionDef(function) => {
            for inner in &function.body {
                collect_from_stmt(inner, locator, out, opaque);
            }
        },
        Stmt::AsyncFunctionDef(function) => {
            for inner in &function.body {
                collect_from_stmt(inner, locator, out, opaque);
            }
        },
        Stmt::ClassDef(class) => {
            for inner in &class.body {
                collect_from_stmt(inner, locator, out, opaque);
            }
        },
        Stmt::If(if_stmt) => {
            for inner in &if_stmt.body {
                collect_from_stmt(inner, locator, out, opaque);
            }
            for inner in &if_stmt.orelse {
                collect_from_stmt(inner, locator, out, opaque);
            }
        },
        Stmt::Try(try_stmt) => {
            for inner in &try_stmt.body {
                collect_from_stmt(inner, locator, out, opaque);
            }
            for handler in &try_stmt.handlers {
                let rustpython_parser::ast::ExceptHandler::ExceptHandler(handler) = handler;
                for inner in &handler.body {
                    collect_from_stmt(inner, locator, out, opaque);
                }
            }
            for inner in &try_stmt.orelse {
                collect_from_stmt(inner, locator, out, opaque);
            }
            for inner in &try_stmt.finalbody {
                collect_from_stmt(inner, locator, out, opaque);
            }
        },
        Stmt::With(with_stmt) => {
            for inner in &with_stmt.body {
                collect_from_stmt(inner, locator, out, opaque);
            }
        },
        Stmt::AsyncWith(with_stmt) => {
            for inner in &with_stmt.body {
                collect_from_stmt(inner, locator, out, opaque);
            }
        },
        Stmt::Match(match_stmt) => {
            for case in &match_stmt.cases {
                for inner in &case.body {
                    collect_from_stmt(inner, locator, out, opaque);
                }
            }
        },
        Stmt::For(for_stmt) => {
            for inner in &for_stmt.body {
                collect_from_stmt(inner, locator, out, opaque);
            }
            for inner in &for_stmt.orelse {
                collect_from_stmt(inner, locator, out, opaque);
            }
        },
        Stmt::AsyncFor(for_stmt) => {
            for inner in &for_stmt.body {
                collect_from_stmt(inner, locator, out, opaque);
            }
            for inner in &for_stmt.orelse {
                collect_from_stmt(inner, locator, out, opaque);
            }
        },
        Stmt::While(while_stmt) => {
            for inner in &while_stmt.body {
                collect_from_stmt(inner, locator, out, opaque);
            }
            for inner in &while_stmt.orelse {
                collect_from_stmt(inner, locator, out, opaque);
            }
        },
        Stmt::Expr(expr_stmt) => {
            inspect_expr(&expr_stmt.value, locator, out, opaque);
        },
        _ => {},
    }
}

fn inspect_expr(
    expr: &Expr,
    locator: &mut RandomLocator<'_>,
    out: &mut Vec<DynamicImport>,
    opaque: &mut bool,
) {
    if let Expr::Call(call) = expr {
        if let Some(module) = extract_literal_module_call(&call.func, &call.args) {
            let line = locator.locate(call.start()).row.get();
            out.push(DynamicImport { module, line });
            return;
        }
        if is_import_module_call(&call.func) && !call.args.is_empty() {
            *opaque = true;
        }
    }
}

fn extract_literal_module_call(func: &Expr, args: &[Expr]) -> Option<String> {
    if !is_import_module_call(func) {
        return None;
    }
    string_literal(args.first()?)
}

fn is_import_module_call(func: &Expr) -> bool {
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

#[cfg(test)]
mod tests {
    use rustpython_parser::Parse;
    use rustpython_parser::ast::Suite;

    use super::*;

    #[test]
    fn extracts_importlib_literal() {
        let source = "import importlib\nimportlib.import_module(\"acme.plugins\")\n";
        let stmts = Suite::parse(source, "<test>").expect("parse");
        let mut imports = Vec::new();
        let mut opaque = false;
        let mut locator = RandomLocator::new(source);
        collect_dynamic_imports(&stmts, &mut locator, &mut imports, &mut opaque);
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].module, "acme.plugins");
        assert_eq!(imports[0].line, 2);
        assert!(!opaque);
    }
}
