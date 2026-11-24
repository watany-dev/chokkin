//! The names a function, class, lambda or comprehension binds itself, which
//! hide a module-level import of the same name inside it (#1429).

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_pattern, walk_stmt};
use ruff_python_ast::{
    Comprehension, ExceptHandler, Expr, ExprContext, Identifier, Parameters, Pattern, Stmt,
    TypeParams,
};
use rustc_hash::FxHashSet;

/// One enclosing scope of the walk.
#[derive(Default)]
pub(super) struct Scope<'ast> {
    /// Names the scope binds other than by an import statement.
    locals: FxHashSet<&'ast str>,
    /// Names a `global` statement declares, which read the module's binding.
    globals: FxHashSet<&'ast str>,
    /// A class body, whose names nested scopes do not see.
    class: bool,
}

impl<'ast> Scope<'ast> {
    /// A function's (or lambda's) scope: its type parameters, parameters and
    /// what its body binds.
    pub(super) fn function(
        type_params: Option<&'ast TypeParams>,
        parameters: Option<&'ast Parameters>,
        body: &'ast [Stmt],
    ) -> Self {
        let mut names = LocalNames::default();
        for param in type_params.into_iter().flat_map(|params| params.iter()) {
            names.locals.insert(param.name().as_str());
        }
        for parameter in parameters.into_iter().flatten() {
            names.locals.insert(parameter.name().as_str());
        }
        names.visit_body(body);
        names.into_scope(false)
    }

    pub(super) fn lambda(parameters: Option<&'ast Parameters>, body: &'ast Expr) -> Self {
        let mut names = LocalNames::default();
        for parameter in parameters.into_iter().flatten() {
            names.locals.insert(parameter.name().as_str());
        }
        names.visit_expr(body);
        names.into_scope(false)
    }

    pub(super) fn class(body: &'ast [Stmt]) -> Self {
        let mut names = LocalNames::default();
        names.visit_body(body);
        names.into_scope(true)
    }

    /// The targets of a comprehension's `for` clauses.
    pub(super) fn comprehension(generators: &'ast [Comprehension]) -> Self {
        let mut names = LocalNames::default();
        for generator in generators {
            names.bind_target(&generator.target);
        }
        names.into_scope(false)
    }

    pub(super) fn binds(&self, name: &str) -> bool {
        self.locals.contains(name)
    }
}

/// Whether `name` read in the innermost of `scopes` is a name one of them
/// binds rather than the module's. A class body is seen only from itself.
pub(super) fn is_shadowed(scopes: &[Scope<'_>], name: &str) -> bool {
    let innermost = scopes.len().saturating_sub(1);
    for (index, scope) in scopes.iter().enumerate().rev() {
        if scope.class && index != innermost {
            continue;
        }
        if scope.globals.contains(name) {
            return false;
        }
        if scope.locals.contains(name) {
            return true;
        }
    }
    false
}

/// What one scope's body binds. Nested functions, classes, lambdas and
/// comprehensions bind in scopes of their own. A name the scope also binds
/// by an import is left out: symbol tracking reads every import of the
/// module as its receiver, and `except ImportError: yaml = None` keeps the
/// import's binding on the other path.
#[derive(Default)]
struct LocalNames<'ast> {
    locals: FxHashSet<&'ast str>,
    imported: FxHashSet<&'ast str>,
    globals: FxHashSet<&'ast str>,
    nonlocals: FxHashSet<&'ast str>,
}

impl<'ast> LocalNames<'ast> {
    fn into_scope(mut self, class: bool) -> Scope<'ast> {
        // `nonlocal x` names the enclosing function's `x`, whose own scope
        // has it.
        self.locals.retain(|name| {
            !self.imported.contains(name)
                && !self.globals.contains(name)
                && !self.nonlocals.contains(name)
        });
        Scope {
            locals: self.locals,
            globals: self.globals,
            class,
        }
    }

    fn bind_target(&mut self, target: &'ast Expr) {
        match target {
            Expr::Name(name) => {
                self.locals.insert(name.id.as_str());
            },
            Expr::Tuple(tuple) => tuple.elts.iter().for_each(|elt| self.bind_target(elt)),
            Expr::List(list) => list.elts.iter().for_each(|elt| self.bind_target(elt)),
            Expr::Starred(starred) => self.bind_target(&starred.value),
            _ => {},
        }
    }
}

impl<'ast> Visitor<'ast> for LocalNames<'ast> {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        match stmt {
            Stmt::Import(import) => {
                for alias in &import.names {
                    let name = alias.asname.as_ref().map_or_else(
                        || alias.name.split('.').next().unwrap_or_default(),
                        Identifier::as_str,
                    );
                    self.imported.insert(name);
                }
            },
            Stmt::ImportFrom(import) => {
                for alias in &import.names {
                    self.imported
                        .insert(alias.asname.as_ref().unwrap_or(&alias.name).as_str());
                }
            },
            Stmt::FunctionDef(def) => {
                self.locals.insert(def.name.as_str());
            },
            Stmt::ClassDef(def) => {
                self.locals.insert(def.name.as_str());
            },
            Stmt::Global(global) => self
                .globals
                .extend(global.names.iter().map(Identifier::as_str)),
            Stmt::Nonlocal(nonlocal) => self
                .nonlocals
                .extend(nonlocal.names.iter().map(Identifier::as_str)),
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_except_handler(&mut self, handler: &'ast ExceptHandler) {
        let ExceptHandler::ExceptHandler(inner) = handler;
        if let Some(name) = &inner.name {
            self.locals.insert(name.as_str());
        }
        self.visit_body(&inner.body);
    }

    fn visit_pattern(&mut self, pattern: &'ast Pattern) {
        let name = match pattern {
            Pattern::MatchAs(pattern) => pattern.name.as_ref(),
            Pattern::MatchStar(pattern) => pattern.name.as_ref(),
            Pattern::MatchMapping(pattern) => pattern.rest.as_ref(),
            _ => None,
        };
        if let Some(name) = name {
            self.locals.insert(name.as_str());
        }
        walk_pattern(self, pattern);
    }

    fn visit_comprehension(&mut self, comprehension: &'ast Comprehension) {
        // The target binds in the comprehension's scope; a walrus in it
        // binds here.
        self.visit_expr(&comprehension.iter);
        for condition in &comprehension.ifs {
            self.visit_expr(condition);
        }
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            Expr::Name(name) if name.ctx != ExprContext::Load => {
                self.locals.insert(name.id.as_str());
            },
            Expr::Lambda(_) => {},
            _ => walk_expr(self, expr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function_scope(source: &str) -> Scope<'static> {
        let module = Box::leak(Box::new(
            ruff_python_parser::parse_module(source).expect("parse"),
        ));
        let Stmt::FunctionDef(def) = &module.suite()[0] else {
            panic!("not a function");
        };
        Scope::function(def.type_params.as_deref(), Some(&def.parameters), &def.body)
    }

    #[test]
    fn binds_parameters_assignments_and_handlers_but_not_imports() {
        let scope = function_scope(
            "def f[t](a, *b, c=1, **d):\n    e = 1\n    for g in x: pass\n    with y as h: pass\n    try: pass\n    except E as i: pass\n    match z:\n        case [j, *k]: pass\n    import imp\n    from m import fimp\n    def inner(): pass\n    [l for l in x]\n    (n := 1)\n",
        );
        for name in [
            "t", "a", "b", "c", "d", "e", "g", "h", "i", "j", "k", "inner", "n",
        ] {
            assert!(scope.binds(name), "{name}");
        }
        for name in ["imp", "fimp", "l", "x"] {
            assert!(!scope.binds(name), "{name}");
        }
        let fallback = function_scope(
            "def f():\n    try:\n        import yaml\n    except ImportError:\n        yaml = None\n",
        );
        assert!(!fallback.binds("yaml"));
    }

    #[test]
    fn global_and_nonlocal_names_are_not_local() {
        let scope = function_scope("def f():\n    global g\n    nonlocal n\n    g = n = 1\n");
        assert!(!scope.binds("g"));
        assert!(!scope.binds("n"));
        assert!(!is_shadowed(&[scope], "g"));
    }

    #[test]
    fn class_names_are_seen_only_from_the_class_body() {
        let module = ruff_python_parser::parse_module("class C:\n    pkg = 1\n").expect("parse");
        let Stmt::ClassDef(def) = &module.suite()[0] else {
            panic!("not a class");
        };
        let class = Scope::class(&def.body);
        assert!(is_shadowed(std::slice::from_ref(&class), "pkg"));
        let method = Scope::default();
        assert!(!is_shadowed(&[class, method], "pkg"));
    }
}
