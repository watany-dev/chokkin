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
        Stmt::Assign(assign) => {
            inspect_expr(&assign.value, locator, out, opaque);
        },
        Stmt::AnnAssign(ann_assign) => {
            if let Some(value) = &ann_assign.value {
                inspect_expr(value, locator, out, opaque);
            }
        },
        Stmt::AugAssign(aug_assign) => {
            inspect_expr(&aug_assign.value, locator, out, opaque);
        },
        Stmt::Return(return_stmt) => {
            if let Some(value) = &return_stmt.value {
                inspect_expr(value, locator, out, opaque);
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
    match expr {
        Expr::Call(call) => {
            if let Some(module) = extract_literal_module_call(&call.func, &call.args) {
                let line = locator.locate(call.start()).row.get();
                out.push(DynamicImport { module, line });
            } else if is_import_module_call(&call.func) && !call.args.is_empty() {
                *opaque = true;
            }
            inspect_expr(&call.func, locator, out, opaque);
            for arg in &call.args {
                inspect_expr(arg, locator, out, opaque);
            }
            for keyword in &call.keywords {
                inspect_expr(&keyword.value, locator, out, opaque);
            }
        },
        Expr::BoolOp(bool_op) => {
            for value in &bool_op.values {
                inspect_expr(value, locator, out, opaque);
            }
        },
        Expr::NamedExpr(named) => {
            inspect_expr(&named.value, locator, out, opaque);
        },
        Expr::BinOp(bin_op) => {
            inspect_expr(&bin_op.left, locator, out, opaque);
            inspect_expr(&bin_op.right, locator, out, opaque);
        },
        Expr::UnaryOp(unary) => {
            inspect_expr(&unary.operand, locator, out, opaque);
        },
        Expr::IfExp(if_exp) => {
            inspect_expr(&if_exp.test, locator, out, opaque);
            inspect_expr(&if_exp.body, locator, out, opaque);
            inspect_expr(&if_exp.orelse, locator, out, opaque);
        },
        Expr::Dict(dict) => {
            for (key, value) in dict.keys.iter().zip(&dict.values) {
                if let Some(key) = key {
                    inspect_expr(key, locator, out, opaque);
                }
                inspect_expr(value, locator, out, opaque);
            }
        },
        Expr::Set(set) => {
            for value in &set.elts {
                inspect_expr(value, locator, out, opaque);
            }
        },
        Expr::ListComp(list_comp) => {
            inspect_expr(&list_comp.elt, locator, out, opaque);
            for comprehension in &list_comp.generators {
                inspect_expr(&comprehension.target, locator, out, opaque);
                inspect_expr(&comprehension.iter, locator, out, opaque);
                for if_clause in &comprehension.ifs {
                    inspect_expr(if_clause, locator, out, opaque);
                }
            }
        },
        Expr::SetComp(set_comp) => {
            inspect_expr(&set_comp.elt, locator, out, opaque);
            for comprehension in &set_comp.generators {
                inspect_expr(&comprehension.target, locator, out, opaque);
                inspect_expr(&comprehension.iter, locator, out, opaque);
                for if_clause in &comprehension.ifs {
                    inspect_expr(if_clause, locator, out, opaque);
                }
            }
        },
        Expr::DictComp(dict_comp) => {
            inspect_expr(&dict_comp.key, locator, out, opaque);
            inspect_expr(&dict_comp.value, locator, out, opaque);
            for comprehension in &dict_comp.generators {
                inspect_expr(&comprehension.target, locator, out, opaque);
                inspect_expr(&comprehension.iter, locator, out, opaque);
                for if_clause in &comprehension.ifs {
                    inspect_expr(if_clause, locator, out, opaque);
                }
            }
        },
        Expr::GeneratorExp(generator) => {
            inspect_expr(&generator.elt, locator, out, opaque);
            for comprehension in &generator.generators {
                inspect_expr(&comprehension.target, locator, out, opaque);
                inspect_expr(&comprehension.iter, locator, out, opaque);
                for if_clause in &comprehension.ifs {
                    inspect_expr(if_clause, locator, out, opaque);
                }
            }
        },
        Expr::Await(await_expr) => {
            inspect_expr(&await_expr.value, locator, out, opaque);
        },
        Expr::Yield(yield_expr) => {
            if let Some(value) = &yield_expr.value {
                inspect_expr(value, locator, out, opaque);
            }
        },
        Expr::YieldFrom(yield_from) => {
            inspect_expr(&yield_from.value, locator, out, opaque);
        },
        Expr::Compare(compare) => {
            inspect_expr(&compare.left, locator, out, opaque);
            for comparator in &compare.comparators {
                inspect_expr(comparator, locator, out, opaque);
            }
        },
        Expr::Attribute(attribute) => {
            inspect_expr(&attribute.value, locator, out, opaque);
        },
        Expr::Subscript(subscript) => {
            inspect_expr(&subscript.value, locator, out, opaque);
            inspect_expr(&subscript.slice, locator, out, opaque);
        },
        Expr::Starred(starred) => {
            inspect_expr(&starred.value, locator, out, opaque);
        },
        Expr::List(list) => {
            for value in &list.elts {
                inspect_expr(value, locator, out, opaque);
            }
        },
        Expr::Tuple(tuple) => {
            for value in &tuple.elts {
                inspect_expr(value, locator, out, opaque);
            }
        },
        Expr::FormattedValue(formatted) => {
            inspect_expr(&formatted.value, locator, out, opaque);
        },
        Expr::JoinedStr(joined) => {
            for value in &joined.values {
                inspect_expr(value, locator, out, opaque);
            }
        },
        Expr::Slice(slice) => {
            if let Some(lower) = &slice.lower {
                inspect_expr(lower, locator, out, opaque);
            }
            if let Some(upper) = &slice.upper {
                inspect_expr(upper, locator, out, opaque);
            }
            if let Some(step) = &slice.step {
                inspect_expr(step, locator, out, opaque);
            }
        },
        Expr::Lambda(lambda) => {
            inspect_expr(&lambda.body, locator, out, opaque);
        },
        Expr::Constant(_) | Expr::Name(_) => {},
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

    #[test]
    fn extracts_importlib_from_assignment() {
        let source = "import importlib\nmod = importlib.import_module(\"acme.plugins\")\n";
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

    #[test]
    fn extracts_importlib_from_return() {
        let source = "import importlib\ndef load():\n    return importlib.import_module(\"acme.plugins\")\n";
        let stmts = Suite::parse(source, "<test>").expect("parse");
        let mut imports = Vec::new();
        let mut opaque = false;
        let mut locator = RandomLocator::new(source);
        collect_dynamic_imports(&stmts, &mut locator, &mut imports, &mut opaque);
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].module, "acme.plugins");
        assert_eq!(imports[0].line, 3);
        assert!(!opaque);
    }

    #[test]
    fn extracts_importlib_from_call_argument() {
        let source = "import importlib\ndef run(fn):\n    pass\nrun(importlib.import_module(\"acme.plugins\"))\n";
        let stmts = Suite::parse(source, "<test>").expect("parse");
        let mut imports = Vec::new();
        let mut opaque = false;
        let mut locator = RandomLocator::new(source);
        collect_dynamic_imports(&stmts, &mut locator, &mut imports, &mut opaque);
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].module, "acme.plugins");
        assert_eq!(imports[0].line, 4);
        assert!(!opaque);
    }

    #[test]
    fn marks_opaque_assignment_with_non_literal() {
        let source = "import importlib\nmod = importlib.import_module(name)\n";
        let stmts = Suite::parse(source, "<test>").expect("parse");
        let mut imports = Vec::new();
        let mut opaque = false;
        let mut locator = RandomLocator::new(source);
        collect_dynamic_imports(&stmts, &mut locator, &mut imports, &mut opaque);
        assert!(imports.is_empty());
        assert!(opaque);
    }
}
