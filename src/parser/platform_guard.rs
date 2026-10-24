//! Platform guard detection for conditional imports: `sys.platform`,
//! `os.name` and `platform.system()` tests (#957).

use rustc_hash::FxHashMap as HashMap;

use ruff_python_ast::visitor::{Visitor, walk_except_handler, walk_expr, walk_pattern, walk_stmt};
use ruff_python_ast::{CmpOp, ExceptHandler, Expr, ExprContext, Parameters, Pattern, Stmt};

/// A stdlib module whose attributes tell the platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlatformModule {
    Sys,
    Os,
    Platform,
}

impl PlatformModule {
    fn from_import(module: &str) -> Option<Self> {
        match module {
            "sys" => Some(Self::Sys),
            "os" => Some(Self::Os),
            "platform" => Some(Self::Platform),
            _ => None,
        }
    }

    /// The attribute a test reads off the module: `sys.platform`, `os.name`,
    /// `platform.system()`.
    fn attribute(self) -> &'static str {
        match self {
            Self::Sys => "platform",
            Self::Os => "name",
            Self::Platform => "system",
        }
    }
}

pub(super) type PlatformModules = HashMap<String, PlatformModule>;

/// The names `body` binds to a platform module, starting from `outer`, the
/// enclosing scope's (`None` at module level). A name every binding of the
/// scope imports the same module to counts; a parameter or any other binding
/// shadows the outer one, so its tests are unknown leaves.
pub(super) fn platform_modules(
    outer: Option<&PlatformModules>,
    parameters: Option<&Parameters>,
    body: &[Stmt],
) -> PlatformModules {
    let mut bindings = Bindings::default();
    for parameter in parameters.into_iter().flatten() {
        bindings.bind(parameter.name().as_str(), None);
    }
    bindings.visit_body(body);
    let mut modules = outer.cloned().unwrap_or_default();
    for (name, module) in bindings.0 {
        match module {
            Some(module) => modules.insert(name.to_owned(), module),
            None => modules.remove(name),
        };
    }
    modules
}

/// Whether the scope `body` imports a platform module under another name
/// (`import sys as s`), in a nested block too; a module bound under its own
/// name is found by its `sys.platform`-style mention instead.
pub(super) fn imports_platform_alias(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Import(import) => import.names.iter().any(|alias| {
            alias.asname.is_some() && PlatformModule::from_import(alias.name.as_str()).is_some()
        }),
        Stmt::If(stmt) => {
            imports_platform_alias(&stmt.body)
                || stmt
                    .elif_else_clauses
                    .iter()
                    .any(|clause| imports_platform_alias(&clause.body))
        },
        Stmt::Try(stmt) => {
            imports_platform_alias(&stmt.body)
                || stmt.handlers.iter().any(|handler| {
                    let ExceptHandler::ExceptHandler(handler) = handler;
                    imports_platform_alias(&handler.body)
                })
                || imports_platform_alias(&stmt.orelse)
                || imports_platform_alias(&stmt.finalbody)
        },
        Stmt::With(stmt) => imports_platform_alias(&stmt.body),
        Stmt::For(stmt) => {
            imports_platform_alias(&stmt.body) || imports_platform_alias(&stmt.orelse)
        },
        Stmt::While(stmt) => {
            imports_platform_alias(&stmt.body) || imports_platform_alias(&stmt.orelse)
        },
        Stmt::Match(stmt) => stmt
            .cases
            .iter()
            .any(|case| imports_platform_alias(&case.body)),
        _ => false,
    })
}

/// Whether `source`, an ASCII one, can hold a platform test of `modules`;
/// most modules have none, and the per-scope binding scan can then be skipped.
pub(super) fn mentions_platform_test(source: &str, modules: &PlatformModules) -> bool {
    modules.iter().any(|(name, module)| {
        let needle = format!("{name}.{}", module.attribute());
        memchr::memmem::find(source.as_bytes(), needle.as_bytes()).is_some()
    })
}

/// Per bound name, the module every binding so far imported (`None` once
/// another binding was seen).
#[derive(Default)]
struct Bindings<'a>(HashMap<&'a str, Option<PlatformModule>>);

impl<'a> Bindings<'a> {
    fn bind(&mut self, name: &'a str, module: Option<PlatformModule>) {
        self.0
            .entry(name)
            .and_modify(|bound| {
                if *bound != module {
                    *bound = None;
                }
            })
            .or_insert(module);
    }
}

impl<'a> Visitor<'a> for Bindings<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Import(import) => {
                for alias in &import.names {
                    // `import os.path` binds `os` to `os`.
                    let root = alias.name.split('.').next().unwrap_or_default();
                    let (name, module) = alias.asname.as_ref().map_or((root, root), |asname| {
                        (asname.as_str(), alias.name.as_str())
                    });
                    self.bind(name, PlatformModule::from_import(module));
                }
            },
            Stmt::ImportFrom(import_from) => {
                for alias in &import_from.names {
                    self.bind(alias.asname.as_ref().unwrap_or(&alias.name).as_str(), None);
                }
            },
            // A nested scope's body binds its own names, not this scope's.
            Stmt::FunctionDef(def) => self.bind(def.name.as_str(), None),
            Stmt::ClassDef(def) => self.bind(def.name.as_str(), None),
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_except_handler(&mut self, handler: &'a ExceptHandler) {
        let ExceptHandler::ExceptHandler(inner) = handler;
        if let Some(name) = &inner.name {
            self.bind(name.as_str(), None);
        }
        walk_except_handler(self, handler);
    }

    fn visit_pattern(&mut self, pattern: &'a Pattern) {
        let name = match pattern {
            Pattern::MatchAs(pattern) => pattern.name.as_ref(),
            Pattern::MatchStar(pattern) => pattern.name.as_ref(),
            Pattern::MatchMapping(pattern) => pattern.rest.as_ref(),
            _ => None,
        };
        if let Some(name) = name {
            self.bind(name.as_str(), None);
        }
        walk_pattern(self, pattern);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Name(name) = expr
            && name.ctx != ExprContext::Load
        {
            self.bind(name.id.as_str(), None);
        } else {
            walk_expr(self, expr);
        }
    }
}

/// Returns `true` when `expr` tests the platform: `sys.platform`, `os.name`
/// or `platform.system()` compared with `==` / `!=` / ordering / `in`, or
/// `sys.platform.startswith(...)`, with a receiver `modules` binds to that
/// stdlib module. Anything else, a shadowed receiver included, is not one.
#[must_use]
pub(super) fn is_platform_guard_test(expr: &Expr, modules: &PlatformModules) -> bool {
    match expr {
        Expr::Compare(compare) => {
            compare
                .operands
                .iter()
                .any(|operand| is_platform_value(operand, modules))
                && compare
                    .ops
                    .iter()
                    .all(|op| !matches!(op, CmpOp::Is | CmpOp::IsNot))
        },
        Expr::Call(call) => {
            !call.arguments.args.is_empty()
                && matches!(
                    &*call.func,
                    Expr::Attribute(method) if method.attr.as_str() == "startswith"
                        && module_attribute(&method.value, modules) == Some(PlatformModule::Sys)
                )
        },
        _ => false,
    }
}

/// `sys.platform`, `os.name` or `platform.system()`.
fn is_platform_value(expr: &Expr, modules: &PlatformModules) -> bool {
    match expr {
        Expr::Call(call) => {
            call.arguments.is_empty()
                && module_attribute(&call.func, modules) == Some(PlatformModule::Platform)
        },
        _ => matches!(
            module_attribute(expr, modules),
            Some(PlatformModule::Sys | PlatformModule::Os)
        ),
    }
}

/// The module whose platform attribute `expr` reads: `sys.platform` is
/// `Sys`, `os.name` `Os`, `platform.system` `Platform`.
fn module_attribute(expr: &Expr, modules: &PlatformModules) -> Option<PlatformModule> {
    let Expr::Attribute(attribute) = expr else {
        return None;
    };
    let Expr::Name(receiver) = &*attribute.value else {
        return None;
    };
    modules
        .get(receiver.id.as_str())
        .copied()
        .filter(|module| module.attribute() == attribute.attr.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_platform_guard(source: &str) -> bool {
        let header = "import os\nimport platform\nimport sys\n";
        let module = ruff_python_parser::parse_module(header).expect("parse");
        let modules = platform_modules(None, None, module.suite());
        let parsed = ruff_python_parser::parse_expression(source).expect("parse");
        is_platform_guard_test(parsed.expr(), &modules)
    }

    #[test]
    fn finds_aliased_platform_imports_in_nested_blocks() {
        let aliases = |source: &str| {
            let module = ruff_python_parser::parse_module(source).expect("parse");
            imports_platform_alias(module.suite())
        };
        for source in [
            "import sys as s\n",
            "import json, os as o\n",
            "try:\n    pass\nexcept ImportError:\n    import platform as p\n",
            "if x:\n    pass\nelse:\n    with y:\n        import sys as s\n",
            "try:\n    pass\nexcept E:\n    pass\nelse:\n    import sys as s\n",
            "try:\n    pass\nfinally:\n    import sys as s\n",
            "for x in y:\n    import sys as s\n",
            "for x in y:\n    pass\nelse:\n    import sys as s\n",
            "while x:\n    import sys as s\n",
            "while x:\n    pass\nelse:\n    import sys as s\n",
            "match x:\n    case 1:\n        pass\n    case _:\n        import sys as s\n",
        ] {
            assert!(aliases(source), "{source}");
        }
        for source in [
            "import sys\nimport os.path as osp\n",
            "from sys import platform as plat\n",
            "def f():\n    import sys as s\n",
            "class C:\n    import os as o\n",
        ] {
            assert!(!aliases(source), "{source}");
        }
    }

    #[test]
    fn detects_sys_platform_equality() {
        assert!(is_platform_guard("sys.platform == 'win32'"));
    }

    #[test]
    fn detects_reversed_platform_comparison() {
        assert!(is_platform_guard("'linux' == sys.platform"));
    }

    #[test]
    fn rejects_non_platform_if() {
        assert!(!is_platform_guard("foo == 'bar'"));
    }

    #[test]
    fn detects_every_stdlib_spelling() {
        for source in [
            "sys.platform.startswith('win')",
            "sys.platform.startswith(('linux', 'darwin'))",
            "os.name == 'nt'",
            "'posix' != os.name",
            "platform.system() == 'Windows'",
            "sys.platform in ('win32', 'cygwin')",
        ] {
            assert!(is_platform_guard(source), "{source}");
        }
        for source in [
            "sys.platform.startswith()",
            "os.name.startswith('n')",
            "platform.system('x') == 'Windows'",
            "platform.machine() == 'arm64'",
            "sys.name == 'nt'",
            "os.platform == 'nt'",
            "sys.platform is None",
            "self.platform == 'win32'",
        ] {
            assert!(!is_platform_guard(source), "{source}");
        }
    }

    /// A receiver counts while every binding of its scope imports the module.
    #[test]
    fn collects_names_bound_only_to_the_module() {
        let names = |source: &str| {
            let module = ruff_python_parser::parse_module(source).expect("parse");
            let outer = platform_modules(None, None, module.suite());
            let (parameters, body) = match &**module.suite() {
                [.., Stmt::FunctionDef(def)] => (Some(&*def.parameters), &*def.body),
                _ => (None, &[][..]),
            };
            let mut names: Vec<(String, PlatformModule)> =
                platform_modules(Some(&outer), parameters, body)
                    .into_iter()
                    .collect();
            names.sort_by(|a, b| a.0.cmp(&b.0));
            names
        };
        assert_eq!(
            names("import os.path\nimport sys as system\nimport platform as p\n"),
            [
                ("os".to_owned(), PlatformModule::Os),
                ("p".to_owned(), PlatformModule::Platform),
                ("system".to_owned(), PlatformModule::Sys),
            ]
        );
        assert_eq!(
            names("import sys\ndef f():\n    import os\n"),
            [
                ("os".to_owned(), PlatformModule::Os),
                ("sys".to_owned(), PlatformModule::Sys),
            ]
        );
        assert_eq!(
            names("import sys\ndef f():\n    import os as sys\n"),
            [("sys".to_owned(), PlatformModule::Os)]
        );
        // A class body binds the class's names, not the function's.
        assert_eq!(
            names("import sys\ndef f():\n    class C:\n        sys = 1\n"),
            [("sys".to_owned(), PlatformModule::Sys)]
        );
        for shadowed in [
            "import sys\ndef f(sys):\n    pass\n",
            "import sys\ndef f():\n    sys = 1\n",
            "import sys\ndef f():\n    for sys in x:\n        pass\n",
            "import sys\ndef f():\n    from x import sys\n",
            "import sys\ndef f():\n    try:\n        pass\n    except E as sys:\n        pass\n",
            "import sys\ndef f():\n    def sys():\n        pass\n",
            "import sys\ndef f():\n    class sys:\n        pass\n",
            "import sys\nsys = None\ndef f():\n    pass\n",
            "import os.path as sys\ndef f():\n    pass\n",
            "import sys\ndef f(x):\n    match x:\n        case sys:\n            pass\n",
            "import sys\ndef f(x):\n    match x:\n        case [*sys]:\n            pass\n",
            "import sys\ndef f(x):\n    match x:\n        case {**sys}:\n            pass\n",
            "import sys\ndef f(x):\n    match x:\n        case X() as sys:\n            pass\n",
        ] {
            assert_eq!(names(shadowed), [], "{shadowed}");
        }
    }

    #[test]
    fn mentions_need_the_bound_name_and_attribute() {
        let modules = PlatformModules::from_iter([("s".to_owned(), PlatformModule::Sys)]);
        assert!(mentions_platform_test(
            "if s.platform == 'win32': pass",
            &modules
        ));
        assert!(!mentions_platform_test("print(s.argv, platform)", &modules));
    }
}
