//! First-party registry decorators (#1223): a top-level function that stores
//! the object it decorates in a container and hands it back.
//!
//! netbox's `register_search` and weblate's `register_perm("x")` are the two
//! shapes:
//!
//! ```python
//! def register_search(cls):
//!     registry["search"][label] = cls
//!     return cls
//!
//! def register_perm(*perms):
//!     def wrap(function):
//!         for perm in perms:
//!             SPECIALS[perm] = function
//!         return function
//!     return wrap
//! ```
//!
//! A body in any other shape (wrapping the target, registering it through a
//! helper call, returning something else) is not a registry decorator, so the
//! symbols it decorates stay reported as before.

use ruff_python_ast::{Expr, Parameters, Stmt, StmtFunctionDef};

/// Container methods that store an argument: `REGISTRY.append(cls)`,
/// `HOOKS.setdefault(name, fn)`, a nested registry's `.register(cls)`.
const STORING_METHODS: &[&str] = &["append", "add", "insert", "setdefault", "register"];

/// Names of the top-level functions in `stmts` that register what they
/// decorate, directly or through the decorator they return.
pub(super) fn registry_decorators(stmts: &[Stmt]) -> Vec<String> {
    stmts
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::FunctionDef(function) if registers(function) || returns_registrar(function) => {
                Some(function.name.to_string())
            },
            _ => None,
        })
        .collect()
}

/// `def f(target): <store target>; return target`.
fn registers(function: &StmtFunctionDef) -> bool {
    let Some(target) = first_positional(&function.parameters) else {
        return false;
    };
    returns_name(&function.body, target) && stores(&function.body, target)
}

/// `def factory(...): def inner(target): ...; return inner`, where `inner`
/// registers its target.
fn returns_registrar(function: &StmtFunctionDef) -> bool {
    function.body.iter().any(|stmt| match stmt {
        Stmt::FunctionDef(inner) => {
            returns_name(&function.body, inner.name.as_str()) && registers(inner)
        },
        _ => false,
    })
}

fn first_positional(parameters: &Parameters) -> Option<&str> {
    parameters
        .posonlyargs
        .iter()
        .chain(&parameters.args)
        .next()
        .map(|parameter| parameter.parameter.name.as_str())
}

/// The body ends in `return name`.
fn returns_name(body: &[Stmt], name: &str) -> bool {
    matches!(
        body.last(),
        Some(Stmt::Return(ret)) if ret.value.as_deref().is_some_and(|value| is_name(value, name))
    )
}

/// A statement of `body`, outside nested functions and classes, puts
/// `target` (or an instance of it) in a container: a subscript assignment or
/// a storing method call.
fn stores(body: &[Stmt], target: &str) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Assign(assign) => {
            holds(&assign.value, target)
                && assign
                    .targets
                    .iter()
                    .any(|lhs| matches!(lhs, Expr::Subscript(_)))
        },
        Stmt::Expr(expr) => stores_by_call(&expr.value, target),
        Stmt::For(stmt) => stores(&stmt.body, target),
        Stmt::While(stmt) => stores(&stmt.body, target),
        Stmt::If(stmt) => {
            stores(&stmt.body, target)
                || stmt
                    .elif_else_clauses
                    .iter()
                    .any(|clause| stores(&clause.body, target))
        },
        Stmt::With(stmt) => stores(&stmt.body, target),
        Stmt::Try(stmt) => stores(&stmt.body, target) || stores(&stmt.orelse, target),
        _ => false,
    })
}

/// `container.append(target)` and the other [`STORING_METHODS`], called on
/// something other than the target or its attributes.
fn stores_by_call(expr: &Expr, target: &str) -> bool {
    let Expr::Call(call) = expr else {
        return false;
    };
    let Expr::Attribute(method) = &*call.func else {
        return false;
    };
    STORING_METHODS.contains(&method.attr.as_str())
        && !rooted_at(&method.value, target)
        && call
            .arguments
            .args
            .iter()
            .chain(call.arguments.keywords.iter().map(|keyword| &keyword.value))
            .any(|arg| holds(arg, target))
}

/// `target` itself, or `target(...)`: wger's `REGISTRY[key] = cls()`.
fn holds(expr: &Expr, target: &str) -> bool {
    match expr {
        Expr::Call(call) => is_name(&call.func, target),
        _ => is_name(expr, target),
    }
}

/// `name`, or an attribute or item of it (`cls.items`, `cls.hooks["x"]`).
fn rooted_at(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Attribute(attribute) => rooted_at(&attribute.value, name),
        Expr::Subscript(subscript) => rooted_at(&subscript.value, name),
        _ => is_name(expr, name),
    }
}

fn is_name(expr: &Expr, name: &str) -> bool {
    matches!(expr, Expr::Name(found) if found.id.as_str() == name)
}

#[cfg(test)]
mod tests {
    use super::registry_decorators;

    fn found(source: &str) -> Vec<String> {
        let parsed = ruff_python_parser::parse_module(source).expect("parse");
        registry_decorators(parsed.suite())
    }

    #[test]
    fn recognizes_direct_and_factory_registrars() {
        let source = r#"
def register_search(cls):
    label = cls.label
    registry["search"][label] = cls
    return cls

def notification(cls):
    NOTIFICATIONS.append(cls)
    return cls

def register_handler(cls):
    REGISTRY[cls.name] = cls()
    return cls

def register_perm(*perms):
    def wrap(function):
        for perm in perms:
            SPECIALS[perm] = function
        return function
    return wrap

def hook(name, /):
    if name:
        HOOKS.setdefault(name.__name__, name)
    return name

def queued(cls):
    while not QUEUE:
        QUEUE.insert(0, cls)
    return cls

def locked(cls):
    with LOCK:
        HANDLERS[cls.name] = cls
    return cls

def tried(cls):
    try:
        CHECKS.add(cls)
    except KeyError:
        pass
    return cls

def tried_else(cls):
    try:
        name = cls.name
    except AttributeError:
        name = None
    else:
        BY_NAME[name] = cls
    return cls
"#;
        assert_eq!(
            found(source),
            [
                "register_search",
                "notification",
                "register_handler",
                "register_perm",
                "hook",
                "queued",
                "locked",
                "tried",
                "tried_else",
            ]
        );
    }

    #[test]
    fn rejects_wrappers_and_other_shapes() {
        let source = r#"
def cached(fn):
    @functools.wraps(fn)
    def inner(*args):
        return fn(*args)
    return inner

def logged(cls):
    logger.info(cls)
    return cls

def plain_assign(cls):
    last = cls
    return cls

def stores_other(cls):
    REGISTRY[cls.name] = cls.label
    return cls

def mutates_item(cls):
    cls.hooks["x"].append(cls)
    return cls

def in_handler(cls):
    try:
        pass
    except KeyError:
        FAILED.append(cls)
    return cls

def mutates(cls):
    cls.items.append(cls)
    return cls

def returns_other(cls):
    REGISTRY.append(cls)
    return None

def via_helper(cls):
    _store(cls)
    return cls

def nested_store(cls):
    def later():
        REGISTRY.append(cls)
    return cls

def no_params():
    REGISTRY.append(1)
    return REGISTRY

class Registry:
    def register(self, cls):
        self.items[cls.name] = cls
        return cls
"#;
        assert!(found(source).is_empty(), "{:?}", found(source));
    }
}
