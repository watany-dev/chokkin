//! Shapes that use a name or a file without importing it (#1274): a path
//! handed to a file call, a computed `sys.path` entry, and the module's own
//! namespace read by a computed name.

use std::path::Path;

use ruff_python_ast::{Expr, ExprCall, ExprSubscript};

use super::attributes::is_receiver;
use super::dynamic::str_constant;

/// Callees, by their last name, that take a path: `os.path.join` and the
/// `pathlib` constructors build one, the rest read, copy or run the file.
const PATH_CALLS: &[&str] = &[
    "join",
    "joinpath",
    "Path",
    "PurePath",
    "open",
    "copy",
    "copy2",
    "copyfile",
    "run_path",
    "read_text",
    "read_bytes",
    "isfile",
    "exists",
    "abspath",
    "realpath",
    "normpath",
];

/// The last name of a callee: `join` for `os.path.join`, `open` for `open`.
pub(super) fn callee_name(func: &Expr) -> Option<&str> {
    match func {
        Expr::Name(name) => Some(name.id.as_str()),
        Expr::Attribute(attribute) => Some(attribute.attr.as_str()),
        _ => None,
    }
}

/// The relative `.py` path a run of consecutive string literal arguments of
/// a path call spells (`join(prefix, "driver", "driver.py")` gives
/// `driver/driver.py`, `copyfile("data/x.py", dst)` gives `data/x.py`), when
/// it has a directory: a bare `"setup.py"` names a file in whatever
/// directory the code runs in, which may be any file of that name.
pub(super) fn path_ref(call: &ExprCall) -> Option<String> {
    if !callee_name(&call.func).is_some_and(|name| PATH_CALLS.contains(&name)) {
        return None;
    }
    let args = &call.arguments.args;
    args.split(|arg| str_constant(arg).is_none())
        .find_map(|run| joined_py_path(run.iter().filter_map(str_constant)))
}

/// The literals joined as one relative path, without `.` and `..`
/// segments, if it is a `.py` file below a directory.
fn joined_py_path<'s>(literals: impl Iterator<Item = &'s str>) -> Option<String> {
    let mut segments = Vec::new();
    for literal in literals {
        let path_like = literal
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\'));
        if !path_like || literal.starts_with(['/', '\\']) {
            return None;
        }
        segments.extend(
            literal
                .split(['/', '\\'])
                .filter(|segment| !matches!(*segment, "" | "." | "..")),
        );
    }
    let file = Path::new(segments.last()?);
    (segments.len() >= 2 && file.extension().is_some_and(|extension| extension == "py"))
        .then(|| segments.join("/"))
}

/// Whether a call that edits `sys.path` adds a computed path: the path
/// argument of `insert(i, path)` or of `append` / `addsitedir` /
/// `syspath_prepend` is neither a string literal nor, for `extend`, a list
/// of them. A literal is read as a hint already (#719).
pub(super) fn adds_computed_path(call: &ExprCall) -> bool {
    let index = match &*call.func {
        Expr::Attribute(attribute) if attribute.attr.as_str() == "insert" => 1,
        _ => 0,
    };
    let Some(path) = call.arguments.args.get(index) else {
        return false;
    };
    let literal = |expr: &Expr| str_constant(expr).is_some();
    match path {
        Expr::List(list) => !list.elts.iter().all(literal),
        Expr::Tuple(tuple) => !tuple.elts.iter().all(literal),
        other => !literal(other),
    }
}

/// A read of the module's own namespace by a name it computes:
/// `sys.modules[__name__]`, which hands the module to code that may read any
/// attribute of it, or `globals()[name]`. A store (`globals()[name] = v`)
/// defines a name and reads none; a literal key names its one name.
pub(super) fn reads_own_namespace(subscript: &ExprSubscript) -> bool {
    if !subscript.ctx.is_load() {
        return false;
    }
    let own_module = is_receiver(&subscript.value, "sys.modules")
        && matches!(&*subscript.slice, Expr::Name(name) if name.id.as_str() == "__name__");
    own_module || (is_globals_call(&subscript.value) && str_constant(&subscript.slice).is_none())
}

/// `globals().get(name)` with a computed name, read like `globals()[name]`.
pub(super) fn gets_own_name(call: &ExprCall) -> bool {
    matches!(
        &*call.func,
        Expr::Attribute(attribute) if attribute.attr.as_str() == "get"
            && is_globals_call(&attribute.value)
    ) && call
        .arguments
        .args
        .first()
        .is_some_and(|name| str_constant(name).is_none())
}

/// `globals()`.
fn is_globals_call(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Call(call) if is_receiver(&call.func, "globals")
            && call.arguments.args.is_empty()
            && call.arguments.keywords.is_empty()
    )
}

#[cfg(test)]
mod tests {
    use ruff_python_ast::Expr;

    use super::{adds_computed_path, gets_own_name, path_ref, reads_own_namespace};

    fn expr(source: &str) -> Expr {
        ruff_python_parser::parse_expression(source)
            .expect("parse")
            .into_expr()
    }

    fn call_path(source: &str) -> Option<String> {
        let Expr::Call(call) = expr(source) else {
            panic!("not a call: {source}");
        };
        path_ref(&call)
    }

    #[test]
    fn path_refs_join_trailing_literals_with_a_directory() {
        for (source, path) in [
            (
                r#"os.path.join(test_data_prefix, "fixtures/ir.py")"#,
                "fixtures/ir.py",
            ),
            (
                r#"os.path.join(prefix, "driver", "driver.py")"#,
                "driver/driver.py",
            ),
            (
                r#"shutil.copyfile("./data/../stub/x.py", dst)"#,
                "data/stub/x.py",
            ),
            (r#"Path(root, "pkg\\mod.py")"#, "pkg/mod.py"),
            (r#"runpy.run_path("scripts/gen.py")"#, "scripts/gen.py"),
        ] {
            assert_eq!(call_path(source).as_deref(), Some(path), "{source}");
        }
    }

    #[test]
    fn path_refs_skip_bare_names_other_calls_and_computed_tails() {
        for source in [
            r#"open("native.py", "w")"#,
            r#"os.path.join(d, "setup.py")"#,
            r#"os.path.join("fixtures", name, "ir.py")"#,
            r#"log("fixtures/ir.py")"#,
            r#"os.path.join(d, "fixtures/data.txt")"#,
            r#"os.path.join(d, "/abs/x.py")"#,
            r#"os.path.join(d, "a b/x.py")"#,
            r#"os.path.join(d, "fixtures/.py")"#,
        ] {
            assert_eq!(call_path(source), None, "{source}");
        }
    }

    #[test]
    fn computed_sys_path_entries_are_not_literals() {
        let computed = |source: &str| match expr(source) {
            Expr::Call(call) => adds_computed_path(&call),
            _ => panic!("not a call"),
        };
        assert!(computed("sys.path.insert(0, str(Path.cwd()))"));
        assert!(computed("sys.path.append(SRC_DIR)"));
        assert!(computed("sys.path.extend([here, 'lib'])"));
        assert!(computed("monkeypatch.syspath_prepend(tmp / name)"));
        assert!(!computed("sys.path.insert(0, 'src')"));
        assert!(!computed("sys.path.extend(['a', 'b'])"));
        assert!(!computed("sys.path.insert(0)"));
    }

    #[test]
    fn own_namespace_reads_need_a_computed_name() {
        let subscript = |source: &str| {
            let parsed = ruff_python_parser::parse_module(source).expect("parse");
            let found = parsed.suite().iter().find_map(|stmt| match stmt {
                ruff_python_ast::Stmt::Expr(stmt) => match &*stmt.value {
                    Expr::Subscript(subscript) => Some(reads_own_namespace(subscript)),
                    _ => None,
                },
                ruff_python_ast::Stmt::Assign(assign) => match (&assign.targets[0], &*assign.value)
                {
                    (Expr::Subscript(subscript), _) | (_, Expr::Subscript(subscript)) => {
                        Some(reads_own_namespace(subscript))
                    },
                    _ => None,
                },
                _ => None,
            });
            found.expect("a subscript")
        };
        assert!(subscript("sys.modules[__name__]"));
        assert!(subscript("_ts = sys.modules[__name__]"));
        assert!(subscript("globals()[name]"));
        assert!(!subscript("globals()['X']"));
        assert!(!subscript("globals()[name] = value"));
        assert!(!subscript("sys.modules['x']"));
        assert!(!subscript("sys.modules[name]"));
        assert!(!subscript("other()[name]"));
        assert!(!subscript("ns.globals()[name]"));
        assert!(!subscript("globals(x)[name]"));
        let get = |source: &str| match expr(source) {
            Expr::Call(call) => gets_own_name(&call),
            _ => panic!("not a call"),
        };
        assert!(get("globals().get(name)"));
        assert!(!get("globals().get('X')"));
        assert!(!get("globals().update(values)"));
        assert!(!get("config.get(name)"));
    }
}
