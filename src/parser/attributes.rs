//! Attribute access extraction for `import module; module.name` symbol tracking.

#![allow(clippy::too_many_lines)]

use rustpython_parser::ast::Ranged;
use rustpython_parser::ast::{Expr, Stmt};
use rustpython_parser::source_code::RandomLocator;

use super::types::AttributeAccess;

/// Scan statements for attribute accesses against imported module bindings.
pub fn collect_attribute_accesses(
    stmts: &[Stmt],
    locator: &mut RandomLocator<'_>,
    out: &mut Vec<AttributeAccess>,
) {
    for stmt in stmts {
        collect_from_stmt(stmt, locator, out);
    }
}

#[allow(clippy::too_many_lines)]
fn collect_from_stmt(stmt: &Stmt, locator: &mut RandomLocator<'_>, out: &mut Vec<AttributeAccess>) {
    match stmt {
        Stmt::FunctionDef(function) => {
            for inner in &function.body {
                collect_from_stmt(inner, locator, out);
            }
        },
        Stmt::AsyncFunctionDef(function) => {
            for inner in &function.body {
                collect_from_stmt(inner, locator, out);
            }
        },
        Stmt::ClassDef(class) => {
            for inner in &class.body {
                collect_from_stmt(inner, locator, out);
            }
        },
        Stmt::If(if_stmt) => {
            for inner in &if_stmt.body {
                collect_from_stmt(inner, locator, out);
            }
            for inner in &if_stmt.orelse {
                collect_from_stmt(inner, locator, out);
            }
        },
        Stmt::Try(try_stmt) => {
            for inner in &try_stmt.body {
                collect_from_stmt(inner, locator, out);
            }
            for handler in &try_stmt.handlers {
                let rustpython_parser::ast::ExceptHandler::ExceptHandler(handler) = handler;
                for inner in &handler.body {
                    collect_from_stmt(inner, locator, out);
                }
            }
            for inner in &try_stmt.orelse {
                collect_from_stmt(inner, locator, out);
            }
            for inner in &try_stmt.finalbody {
                collect_from_stmt(inner, locator, out);
            }
        },
        Stmt::With(with_stmt) => {
            for inner in &with_stmt.body {
                collect_from_stmt(inner, locator, out);
            }
        },
        Stmt::AsyncWith(with_stmt) => {
            for inner in &with_stmt.body {
                collect_from_stmt(inner, locator, out);
            }
        },
        Stmt::Match(match_stmt) => {
            for case in &match_stmt.cases {
                for inner in &case.body {
                    collect_from_stmt(inner, locator, out);
                }
            }
        },
        Stmt::For(for_stmt) => {
            for inner in &for_stmt.body {
                collect_from_stmt(inner, locator, out);
            }
            for inner in &for_stmt.orelse {
                collect_from_stmt(inner, locator, out);
            }
        },
        Stmt::AsyncFor(for_stmt) => {
            for inner in &for_stmt.body {
                collect_from_stmt(inner, locator, out);
            }
            for inner in &for_stmt.orelse {
                collect_from_stmt(inner, locator, out);
            }
        },
        Stmt::While(while_stmt) => {
            for inner in &while_stmt.body {
                collect_from_stmt(inner, locator, out);
            }
            for inner in &while_stmt.orelse {
                collect_from_stmt(inner, locator, out);
            }
        },
        Stmt::Expr(expr_stmt) => collect_from_expr(&expr_stmt.value, locator, out),
        Stmt::Assign(assign) => {
            for value in &assign.targets {
                collect_from_expr(value, locator, out);
            }
            collect_from_expr(&assign.value, locator, out);
        },
        Stmt::AnnAssign(assign) => {
            collect_from_expr(&assign.target, locator, out);
            if let Some(value) = &assign.value {
                collect_from_expr(value, locator, out);
            }
        },
        Stmt::Return(return_stmt) => {
            if let Some(value) = &return_stmt.value {
                collect_from_expr(value, locator, out);
            }
        },
        _ => {},
    }
}

#[allow(clippy::too_many_lines)]
fn collect_from_expr(expr: &Expr, locator: &mut RandomLocator<'_>, out: &mut Vec<AttributeAccess>) {
    match expr {
        Expr::Attribute(attribute) => {
            if let Some(receiver) = attribute_receiver(&attribute.value) {
                out.push(AttributeAccess {
                    receiver,
                    name: attribute.attr.to_string(),
                    line: locator.locate(attribute.start()).row.get(),
                });
            }
            collect_from_expr(&attribute.value, locator, out);
        },
        Expr::Call(call) => {
            collect_from_expr(&call.func, locator, out);
            for arg in &call.args {
                collect_from_expr(arg, locator, out);
            }
            for keyword in &call.keywords {
                collect_from_expr(&keyword.value, locator, out);
            }
        },
        Expr::BoolOp(bool_op) => {
            for value in &bool_op.values {
                collect_from_expr(value, locator, out);
            }
        },
        Expr::BinOp(bin_op) => {
            collect_from_expr(&bin_op.left, locator, out);
            collect_from_expr(&bin_op.right, locator, out);
        },
        Expr::UnaryOp(unary_op) => collect_from_expr(&unary_op.operand, locator, out),
        Expr::Lambda(lambda) => collect_from_expr(&lambda.body, locator, out),
        Expr::IfExp(if_exp) => {
            collect_from_expr(&if_exp.test, locator, out);
            collect_from_expr(&if_exp.body, locator, out);
            collect_from_expr(&if_exp.orelse, locator, out);
        },
        Expr::Dict(dict) => {
            for (key, value) in dict.keys.iter().zip(&dict.values) {
                if let Some(key) = key {
                    collect_from_expr(key, locator, out);
                }
                collect_from_expr(value, locator, out);
            }
        },
        Expr::Set(set) => {
            for value in &set.elts {
                collect_from_expr(value, locator, out);
            }
        },
        Expr::ListComp(list_comp) => {
            collect_from_expr(&list_comp.elt, locator, out);
            for comp in &list_comp.generators {
                collect_from_expr(&comp.target, locator, out);
                collect_from_expr(&comp.iter, locator, out);
                for if_expr in &comp.ifs {
                    collect_from_expr(if_expr, locator, out);
                }
            }
        },
        Expr::SetComp(set_comp) => {
            collect_from_expr(&set_comp.elt, locator, out);
            for comp in &set_comp.generators {
                collect_from_expr(&comp.target, locator, out);
                collect_from_expr(&comp.iter, locator, out);
                for if_expr in &comp.ifs {
                    collect_from_expr(if_expr, locator, out);
                }
            }
        },
        Expr::DictComp(dict_comp) => {
            collect_from_expr(&dict_comp.key, locator, out);
            collect_from_expr(&dict_comp.value, locator, out);
            for comp in &dict_comp.generators {
                collect_from_expr(&comp.target, locator, out);
                collect_from_expr(&comp.iter, locator, out);
                for if_expr in &comp.ifs {
                    collect_from_expr(if_expr, locator, out);
                }
            }
        },
        Expr::GeneratorExp(generator) => {
            collect_from_expr(&generator.elt, locator, out);
            for comp in &generator.generators {
                collect_from_expr(&comp.target, locator, out);
                collect_from_expr(&comp.iter, locator, out);
                for if_expr in &comp.ifs {
                    collect_from_expr(if_expr, locator, out);
                }
            }
        },
        Expr::List(list) => {
            for value in &list.elts {
                collect_from_expr(value, locator, out);
            }
        },
        Expr::Tuple(tuple) => {
            for value in &tuple.elts {
                collect_from_expr(value, locator, out);
            }
        },
        Expr::Subscript(subscript) => {
            collect_from_expr(&subscript.value, locator, out);
            collect_from_expr(&subscript.slice, locator, out);
        },
        Expr::Compare(compare) => {
            collect_from_expr(&compare.left, locator, out);
            for comparator in &compare.comparators {
                collect_from_expr(comparator, locator, out);
            }
        },
        _ => {},
    }
}

fn attribute_receiver(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(name) => Some(name.id.to_string()),
        Expr::Attribute(attribute) => {
            let parent = attribute_receiver(&attribute.value)?;
            Some(format!("{}.{}", parent, attribute.attr))
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rustpython_parser::Parse;
    use rustpython_parser::ast;

    use super::*;

    #[test]
    fn collects_module_attribute_accesses() {
        let source = "import acme.utils\nacme.utils.helper()\nvalue = acme.utils.CONFIG\n";
        let stmts = ast::Suite::parse(source, "<test>").expect("parse");
        let mut locator = rustpython_parser::source_code::RandomLocator::new(source);
        let mut accesses = Vec::new();
        collect_attribute_accesses(&stmts, &mut locator, &mut accesses);
        assert!(accesses.iter().any(|access| {
            access.receiver == "acme.utils" && access.name == "helper" && access.line == 2
        }));
        assert!(accesses.iter().any(|access| {
            access.receiver == "acme.utils" && access.name == "CONFIG" && access.line == 3
        }));
    }
}
