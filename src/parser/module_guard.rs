//! Branch conditions that tell an import is not a runtime requirement:
//! "already imported" checks and the `__main__` script block (#681), and
//! "is it installed" checks (#695).

use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{BoolOp, CmpOp, Expr, ExprContext, Parameters, Stmt, UnaryOp};
use ruff_text_size::{Ranged, TextSize};

/// The top-level module `test` proves is already imported when its body
/// runs: `"x" in sys.modules` or `sniffio.current_async_library() == "x"`,
/// the call possibly read back from one of `async_library_names`, or an
/// `is_x…` flag or predicate (django's `is_psycopg3`, streamlit's
/// `is_sympy_expression(body)`) that only holds once `x` is loaded (#781).
#[must_use]
pub(super) fn imported_module_guard(
    test: &Expr,
    async_library_names: &HashSet<String>,
) -> Option<String> {
    let compare = match test {
        Expr::Compare(compare) => compare,
        Expr::Call(call) => return predicate_module(&call.func),
        test => return predicate_module(test),
    };
    let ([op], [left, right]) = (&*compare.ops, &*compare.operands) else {
        return None;
    };
    let module = match op {
        CmpOp::In if is_dotted(right, "sys", "modules") => string_literal(left),
        CmpOp::Eq if is_async_library(left, async_library_names) => string_literal(right),
        CmpOp::Eq if is_async_library(right, async_library_names) => string_literal(left),
        _ => None,
    }?;
    module
        .split('.')
        .next()
        .filter(|root| !root.is_empty())
        .map(str::to_owned)
}

/// `x` of an `is_x` or `is_x_…` name: the module a type predicate or flag
/// names, up to its first `_`. It keeps a version digit (`psycopg3`), which
/// the import's root may drop. A `self.` / `cls.` attribute is the
/// object's own feature (mlflow's `self._is_azure_ad`), as in
/// [`is_availability_call`].
fn predicate_module(expr: &Expr) -> Option<String> {
    let name = match expr {
        Expr::Name(name) => name.id.as_str(),
        Expr::Attribute(attribute) if is_module_path(&attribute.value) => attribute.attr.as_str(),
        _ => return None,
    };
    predicate_name_module(name).map(str::to_owned)
}

/// `x` of an `is_x` or `is_x_…` name, as [`predicate_module`] reads it.
#[must_use]
pub(crate) fn predicate_name_module(name: &str) -> Option<&str> {
    let rest = name.trim_start_matches('_').strip_prefix("is_")?;
    let module = rest.split('_').next()?;
    module
        .starts_with(|c: char| !c.is_ascii_digit())
        .then_some(module)
}

/// Import roots `guard`, a module an [`imported_module_guard`] names, stands
/// for: `is_psycopg3` guards `psycopg`, and `is_boto3_client` `boto3`.
pub(crate) fn guarded_roots(guard: &str) -> [&str; 2] {
    [guard, guard.trim_end_matches(|c: char| c.is_ascii_digit())]
}

/// Names the scope `body` only ever binds to `sniffio.current_async_library()`
/// (`library = sniffio.current_async_library()`), so comparing one compares
/// the call (#731). A parameter is a binding of the function's scope too, and
/// nested scopes have their own names.
#[must_use]
pub(super) fn async_library_names(
    parameters: Option<&Parameters>,
    body: &[Stmt],
) -> HashSet<String> {
    let mut bindings = AsyncLibraryBindings::default();
    for parameter in parameters.into_iter().flatten() {
        bindings
            .only_async_library
            .insert(parameter.name().as_str(), false);
    }
    bindings.visit_body(body);
    bindings
        .only_async_library
        .into_iter()
        .filter(|&(_, only)| only)
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// Per bound name, whether every binding so far assigned the sniffio call.
#[derive(Default)]
struct AsyncLibraryBindings<'a> {
    only_async_library: HashMap<&'a str, bool>,
}

impl<'a> Visitor<'a> for AsyncLibraryBindings<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if let Stmt::Assign(assign) = stmt
            && let [Expr::Name(name)] = &*assign.targets
            && is_async_library_call(&assign.value)
        {
            self.only_async_library
                .entry(name.id.as_str())
                .or_insert(true);
        } else if !matches!(stmt, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            walk_stmt(self, stmt);
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Name(name) = expr
            && name.ctx != ExprContext::Load
        {
            self.only_async_library.insert(name.id.as_str(), false);
        } else {
            walk_expr(self, expr);
        }
    }
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

/// Where a function `body` first checks if a package is installed, outside
/// nested scopes: the start of the innermost statement making the check, so
/// `import_module(x) if find_spec(x) else None` counts as after it (#695).
#[must_use]
pub(super) fn first_availability_check(body: &[Stmt]) -> Option<TextSize> {
    let mut finder = AvailabilityCall::default();
    finder.visit_body(body);
    finder.found
}

#[derive(Default)]
struct AvailabilityCall {
    stmt_start: TextSize,
    found: Option<TextSize>,
}

impl Visitor<'_> for AvailabilityCall {
    fn visit_stmt(&mut self, stmt: &Stmt) {
        if self.found.is_none() && !matches!(stmt, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            let outer = std::mem::replace(&mut self.stmt_start, stmt.start());
            walk_stmt(self, stmt);
            self.stmt_start = outer;
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        if let Expr::Call(call) = expr
            && is_availability_call(&call.func)
        {
            self.found = Some(self.stmt_start);
        }
        if self.found.is_none() && !matches!(expr, Expr::Lambda(_)) {
            walk_expr(self, expr);
        }
    }
}

/// `importlib.util.find_spec` (or a bare `find_spec` / `util.find_spec`), or an
/// `is_<x>_available` function; a `self.` / `cls.` method checks the
/// object's own feature, and another object's `find_spec` is an import-system
/// finder (#713).
fn is_availability_call(func: &Expr) -> bool {
    match func {
        Expr::Name(name) => name.id.as_str() == "find_spec" || is_availability_name(&name.id),
        Expr::Attribute(attribute) if attribute.attr.as_str() == "find_spec" => {
            is_dotted(&attribute.value, "importlib", "util")
                || matches!(&*attribute.value, Expr::Name(name) if name.id.as_str() == "util")
        },
        Expr::Attribute(attribute) => {
            is_availability_name(&attribute.attr) && is_module_path(&attribute.value)
        },
        _ => false,
    }
}

fn is_availability_name(name: &str) -> bool {
    name.trim_start_matches('_')
        .strip_prefix("is_")
        .and_then(|rest| rest.strip_suffix("_available"))
        .is_some_and(|package| !package.is_empty())
}

/// A dotted name such as `transformers.utils`, not rooted at `self` / `cls`.
fn is_module_path(expr: &Expr) -> bool {
    match expr {
        Expr::Name(name) => !matches!(name.id.as_str(), "self" | "cls"),
        Expr::Attribute(attribute) => is_module_path(&attribute.value),
        _ => false,
    }
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

fn is_async_library(expr: &Expr, names: &HashSet<String>) -> bool {
    is_async_library_call(expr)
        || matches!(expr, Expr::Name(name) if names.contains(name.id.as_str()))
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
        imported_module_guard(parsed.expr(), &HashSet::from(["library".to_owned()]))
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
    fn detects_is_module_flags_and_predicates() {
        for (source, module) in [
            ("is_psycopg3", "psycopg3"),
            ("is_boto3_client(c)", "boto3"),
            ("is_sympy_expression(body)", "sympy"),
            ("type_util.is_pandas_df(data)", "pandas"),
            ("_is_torch_available()", "torch"),
        ] {
            assert_eq!(guard(source).as_deref(), Some(module), "{source}");
        }
    }

    #[test]
    fn predicate_names_guard_roots_without_version_digits() {
        assert_eq!(predicate_name_module("is_polars_dataframe"), Some("polars"));
        assert_eq!(predicate_name_module("_is_openai_v1"), Some("openai"));
        assert_eq!(predicate_name_module("is_3d"), None);
        assert_eq!(predicate_name_module("is_"), None);
        assert_eq!(predicate_name_module("this_is_x"), None);
        assert_eq!(guarded_roots("psycopg3"), ["psycopg3", "psycopg"]);
        assert_eq!(guarded_roots("polars"), ["polars", "polars"]);
    }

    #[test]
    fn detects_async_library_comparison() {
        for source in [
            "sniffio.current_async_library() == 'trio'",
            "'trio' == sniffio.current_async_library()",
            "library == 'trio'",
            "'trio' == library",
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
            "backend == 'trio'",
            "library != 'trio'",
            "library.name == 'trio'",
            "not is_psycopg3",
            "is_",
            "is_3",
            "isinstance(x, str)",
            "self._is_azure_ad",
            "cls.is_torch_tensor(x)",
        ] {
            assert_eq!(guard(source), None, "{source}");
        }
    }

    /// #731: a name counts only while every binding of its scope assigns the
    /// sniffio call.
    #[test]
    fn collects_names_bound_only_to_the_async_library_call() {
        let names = |source: &str| {
            let module = ruff_python_parser::parse_module(source).expect("parse");
            let (parameters, body) = match &**module.suite() {
                [Stmt::FunctionDef(def)] => (Some(&*def.parameters), &*def.body),
                body => (None, body),
            };
            let mut names: Vec<String> =
                async_library_names(parameters, body).into_iter().collect();
            names.sort();
            names
        };
        let call = "sniffio.current_async_library()";
        assert_eq!(names(&format!("library = {call}\n")), ["library"]);
        assert_eq!(
            names(&format!("def f(backend):\n    library = {call}\n")),
            ["library"]
        );
        assert_eq!(
            names(&format!(
                "if x:\n    a = {call}\n    b = {call}\nelse:\n    a = {call}\n    b = None\n"
            )),
            ["a"]
        );
        for rebound in [
            "library = 'trio'",
            "library += 'x'",
            "for library in backends:\n    pass",
            "with open(path) as library:\n    pass",
            "if (library := detect()):\n    pass",
            "del library",
            "library, other = pair",
        ] {
            assert!(
                names(&format!("library = {call}\n{rebound}\n")).is_empty(),
                "{rebound}"
            );
            assert!(
                names(&format!("{rebound}\nlibrary = {call}\n")).is_empty(),
                "{rebound}"
            );
        }
        for source in [
            format!("library = other = {call}\n"),
            format!("library = {call}.upper()\n"),
            format!("library: str = {call}\n"),
            format!("self.library = {call}\n"),
            "library = current_async_library()\n".to_owned(),
            format!("x = 1\ndef inner():\n    library = {call}\n"),
            format!("def f(library=None):\n    library = {call}\n"),
            format!("def f(*library):\n    library = {call}\n"),
            format!("def f(*, library):\n    library = {call}\n"),
            format!("def f(**library):\n    library = {call}\n"),
            format!("class Backend:\n    library = {call}\n"),
            format!("probe = lambda: (library := {call})\n"),
        ] {
            assert!(names(&source).is_empty(), "{source}");
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
        let checked_at = |source: &str| {
            let module = ruff_python_parser::parse_module(source).expect("parse");
            first_availability_check(module.suite()).map(u32::from)
        };
        assert_eq!(
            checked_at("x = 1\nif not is_wandb_available():\n    raise RuntimeError\n"),
            Some(6)
        );
        assert_eq!(checked_at("for x in y:\n    ok = find_spec(x)\n"), Some(16));
        assert_eq!(
            checked_at("x = 1\nreturn import_module(x) if find_spec(x) else None\n"),
            Some(6)
        );
        assert_eq!(
            checked_at("import bitsandbytes\nif is_loaded_in_4bit:\n    pass\n"),
            None
        );
        assert_eq!(
            checked_at("def inner():\n    return is_x_available()\nf = lambda: find_spec('y')\n"),
            None
        );
    }

    /// #713: a method checks the object's own feature, and a finder's
    /// `find_spec` is not `importlib.util.find_spec`.
    #[test]
    fn rejects_methods_and_other_find_specs() {
        for source in [
            "self.__is_headers_available()",
            "cls.is_torch_available()",
            "self.utils.is_torch_available()",
            "finder.find_spec('x', None)",
            "importlib.find_spec('x')",
            "get().is_torch_available()",
        ] {
            assert_eq!(availability(source), None, "{source}");
        }
        for source in [
            "utils.is_torch_available()",
            "transformers.utils.is_torch_available()",
            "importlib.util.find_spec('x')",
            "util.find_spec('x')",
        ] {
            assert_eq!(availability(source), Some(true), "{source}");
        }
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
