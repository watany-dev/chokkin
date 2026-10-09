//! Branch conditions that tell an import is not a runtime requirement:
//! "already imported" checks and the `__main__` script block (#681), and
//! "is it installed" checks (#695).

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{BoolOp, CmpOp, Expr, Stmt, UnaryOp};

/// The top-level module `test` proves is already imported when its body
/// runs: `"x" in sys.modules` or `sniffio.current_async_library() == "x"`.
#[must_use]
pub(super) fn imported_module_guard(test: &Expr) -> Option<String> {
    let Expr::Compare(compare) = test else {
        return None;
    };
    let ([op], [left, right]) = (&*compare.ops, &*compare.operands) else {
        return None;
    };
    let module = match op {
        CmpOp::In if is_dotted(right, "sys", "modules") => string_literal(left),
        CmpOp::Eq if is_async_library_call(left) => string_literal(right),
        CmpOp::Eq if is_async_library_call(right) => string_literal(left),
        _ => None,
    }?;
    module
        .split('.')
        .next()
        .filter(|root| !root.is_empty())
        .map(str::to_owned)
}

/// Returns `true` for `__name__ == "__main__"` (either operand order).
#[must_use]
pub(super) fn is_main_guard_test(test: &Expr) -> bool {
    let Expr::Compare(compare) = test else {
        return false;
    };
    let ([CmpOp::Eq], [left, right]) = (&*compare.ops, &*compare.operands) else {
        return false;
    };
    let is_name = |expr: &Expr| matches!(expr, Expr::Name(name) if name.id.as_str() == "__name__");
    let is_main = |expr: &Expr| string_literal(expr) == Some("__main__");
    (is_name(left) && is_main(right)) || (is_main(left) && is_name(right))
}

/// Whether `path` always runs with `__name__ == "__main__"`: a package's
/// `__main__.py` (`python -m pkg`) or a notebook.
#[must_use]
pub(super) fn runs_as_main(path: &str) -> bool {
    let path = std::path::Path::new(path);
    path.file_name().is_some_and(|name| name == "__main__.py")
        || path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ipynb"))
}

/// Whether `test` asks if a package is installed: `Some(true)` when its
/// branch runs only with the package (`find_spec("x")`, transformers'
/// `is_torch_available()`), `Some(false)` when only without it
/// (`not is_x_available()`, `find_spec("x") is None`).
#[must_use]
pub(super) fn availability_test(test: &Expr) -> Option<bool> {
    match test {
        Expr::Call(call) if is_availability_call(&call.func) => Some(true),
        Expr::UnaryOp(unary) if unary.op == UnaryOp::Not => {
            availability_test(&unary.operand).map(|installed| !installed)
        },
        Expr::Compare(compare) => {
            let ([op], [left, right]) = (&*compare.ops, &*compare.operands) else {
                return None;
            };
            let installed = match op {
                CmpOp::IsNot => true,
                CmpOp::Is => false,
                _ => return None,
            };
            let ((call, Expr::NoneLiteral(_)) | (Expr::NoneLiteral(_), call)) = (left, right)
            else {
                return None;
            };
            matches!(call, Expr::Call(call) if is_availability_call(&call.func))
                .then_some(installed)
        },
        // `a and b` runs only with `a`; `not a or not b` only without one.
        Expr::BoolOp(bool_op) => {
            let wanted = bool_op.op == BoolOp::And;
            bool_op
                .values
                .iter()
                .any(|value| availability_test(value) == Some(wanted))
                .then_some(wanted)
        },
        _ => None,
    }
}

/// Whether a function `body` checks if a package is installed anywhere
/// outside nested scopes.
#[must_use]
pub(super) fn checks_availability(body: &[Stmt]) -> bool {
    let mut finder = AvailabilityCall(false);
    finder.visit_body(body);
    finder.0
}

struct AvailabilityCall(bool);

impl Visitor<'_> for AvailabilityCall {
    fn visit_stmt(&mut self, stmt: &Stmt) {
        if !self.0 && !matches!(stmt, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            walk_stmt(self, stmt);
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        if let Expr::Call(call) = expr
            && is_availability_call(&call.func)
        {
            self.0 = true;
        }
        if !self.0 && !matches!(expr, Expr::Lambda(_)) {
            walk_expr(self, expr);
        }
    }
}

fn is_availability_call(func: &Expr) -> bool {
    let name = match func {
        Expr::Name(name) => name.id.as_str(),
        Expr::Attribute(attribute) => attribute.attr.as_str(),
        _ => return false,
    };
    name == "find_spec"
        || name
            .trim_start_matches('_')
            .strip_prefix("is_")
            .and_then(|rest| rest.strip_suffix("_available"))
            .is_some_and(|package| !package.is_empty())
}

/// Whether `body` ends by raising `ImportError` / `ModuleNotFoundError`.
#[must_use]
pub(super) fn raises_import_error(body: &[Stmt]) -> bool {
    let Some(Stmt::Raise(raise)) = body.last() else {
        return false;
    };
    let exc = match raise.exc.as_deref() {
        Some(Expr::Call(call)) => &*call.func,
        Some(exc) => exc,
        None => return false,
    };
    matches!(exc, Expr::Name(name) if matches!(name.id.as_str(), "ImportError" | "ModuleNotFoundError"))
}

fn string_literal(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::StringLiteral(literal) => Some(literal.value.to_str()),
        _ => None,
    }
}

fn is_dotted(expr: &Expr, receiver: &str, attr: &str) -> bool {
    matches!(
        expr,
        Expr::Attribute(attribute)
            if attribute.attr.as_str() == attr
                && matches!(&*attribute.value, Expr::Name(name) if name.id.as_str() == receiver)
    )
}

fn is_async_library_call(expr: &Expr) -> bool {
    matches!(expr, Expr::Call(call)
        if call.arguments.is_empty() && is_dotted(&call.func, "sniffio", "current_async_library"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(source: &str) -> Option<String> {
        let parsed = ruff_python_parser::parse_expression(source).expect("parse");
        imported_module_guard(parsed.expr())
    }

    fn is_main(source: &str) -> bool {
        let parsed = ruff_python_parser::parse_expression(source).expect("parse");
        is_main_guard_test(parsed.expr())
    }

    #[test]
    fn detects_sys_modules_membership() {
        assert_eq!(
            guard("'pyspark.sql' in sys.modules").as_deref(),
            Some("pyspark")
        );
    }

    #[test]
    fn detects_async_library_comparison() {
        for source in [
            "sniffio.current_async_library() == 'trio'",
            "'trio' == sniffio.current_async_library()",
        ] {
            assert_eq!(guard(source).as_deref(), Some("trio"), "{source}");
        }
    }

    #[test]
    fn rejects_other_conditions() {
        for source in [
            "'x' not in sys.modules",
            "'x' in sys.modules and y",
            "'x' in modules",
            "sniffio.current_async_library() != 'trio'",
            "other() == 'trio'",
            "'trio' == other()",
            "name in sys.modules",
        ] {
            assert_eq!(guard(source), None, "{source}");
        }
    }

    fn availability(source: &str) -> Option<bool> {
        let parsed = ruff_python_parser::parse_expression(source).expect("parse");
        availability_test(parsed.expr())
    }

    #[test]
    fn detects_availability_checks() {
        for (source, installed) in [
            ("importlib.util.find_spec('wandb') is not None", true),
            ("find_spec('x') is None", false),
            ("None is find_spec('x')", false),
            ("is_torch_available()", true),
            ("not is_optimum_quanto_available()", false),
            ("_is_package_available('x') and other", true),
            ("not is_a_available() or not is_b_available()", false),
        ] {
            assert_eq!(availability(source), Some(installed), "{source}");
        }
        for source in [
            "torch.cuda.is_available()",
            "is_available()",
            "is_torch_greater_or_equal('2.7')",
            "has_torch",
            "find_spec",
            "is_torch_available() or other",
            "not is_torch_available() and other",
            "find_spec('x') == None",
            "~is_torch_available()",
        ] {
            assert_eq!(availability(source), None, "{source}");
        }
    }

    #[test]
    fn detects_availability_checks_in_bodies() {
        let checks = |source: &str| {
            let module = ruff_python_parser::parse_module(source).expect("parse");
            checks_availability(module.suite())
        };
        assert!(checks(
            "x = 1\nif not is_wandb_available():\n    raise RuntimeError\n"
        ));
        assert!(checks("for x in y:\n    ok = find_spec(x)\n"));
        assert!(!checks(
            "import bitsandbytes\nif is_loaded_in_4bit:\n    pass\n"
        ));
        assert!(!checks(
            "def inner():\n    return is_x_available()\nf = lambda: find_spec('y')\n"
        ));
    }

    #[test]
    fn detects_import_error_raises() {
        let raises = |source: &str| {
            let module = ruff_python_parser::parse_module(source).expect("parse");
            raises_import_error(module.suite())
        };
        assert!(raises("x = 1\nraise ImportError('install x')"));
        assert!(raises("raise ModuleNotFoundError"));
        assert!(!raises("raise ValueError('x')"));
        assert!(!raises("raise ImportError('x')\nx = 1"));
        assert!(!raises("raise"));
    }

    #[test]
    fn detects_main_guard() {
        assert!(is_main("__name__ == '__main__'"));
        assert!(is_main("'__main__' == __name__"));
        assert!(!is_main("__name__ != '__main__'"));
        assert!(!is_main("__name__ == 'pkg'"));
        assert!(!is_main("'pkg' == __name__"));
    }

    #[test]
    fn main_runs_for_dunder_main_and_notebooks_only() {
        assert!(runs_as_main("pkg/__main__.py"));
        assert!(runs_as_main("__main__.py"));
        assert!(runs_as_main("src/acme/report.ipynb"));
        assert!(!runs_as_main("pkg/run__main__.py"));
        assert!(!runs_as_main("pkg/cli.py"));
    }
}
