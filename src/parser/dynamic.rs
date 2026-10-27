//! `importlib.import_module` and `__import__` literal recognition.

use std::borrow::Cow;

use ruff_python_ast::{Alias, Expr, ExprCall, ExprNumberLiteral, Number, Operator};

use super::attributes::is_receiver;
use super::relative::resolve_relative_name;
use super::types::ParsedModule;

#[derive(Debug, Clone, Copy)]
pub(super) enum Loader {
    /// `importlib.import_module(name, package=None)`.
    ImportModule,
    /// `__import__(name, globals=None, locals=None, fromlist=(), level=0)`.
    DunderImport,
}

/// Names one module binds to the dynamic import loaders. A module binds one
/// or two, and every call and argument is looked up, so a scan beats hashing
/// each name (#1087).
pub(super) struct LoaderNames {
    /// Names bound to the `importlib` module.
    modules: Vec<String>,
    /// Names bound directly to a loader function; a later binding wins.
    functions: Vec<(String, Loader)>,
}

impl Default for LoaderNames {
    fn default() -> Self {
        Self {
            modules: vec!["importlib".to_owned()],
            functions: vec![("__import__".to_owned(), Loader::DunderImport)],
        }
    }
}

impl LoaderNames {
    /// Track `import importlib as il`.
    pub(super) fn record_import(&mut self, alias: &Alias) {
        if alias.name.as_str() == "importlib"
            && let Some(asname) = &alias.asname
            && !self.modules.iter().any(|name| name == asname.as_str())
        {
            self.modules.push(asname.to_string());
        }
    }

    /// Track `from importlib import import_module [as im]`.
    pub(super) fn record_import_from(&mut self, module: Option<&str>, alias: &Alias) {
        if module == Some("importlib") && alias.name.as_str() == "import_module" {
            let bound = alias.asname.as_ref().unwrap_or(&alias.name);
            match self
                .functions
                .iter_mut()
                .find(|(name, _)| name == bound.as_str())
            {
                Some((_, loader)) => *loader = Loader::ImportModule,
                None => self
                    .functions
                    .push((bound.to_string(), Loader::ImportModule)),
            }
        }
    }

    /// The loader `expr` names: `importlib.import_module` or `__import__`,
    /// aliases included.
    #[must_use]
    pub(super) fn loader(&self, expr: &Expr) -> Option<Loader> {
        match expr {
            Expr::Attribute(attribute) => (attribute.attr.as_str() == "import_module"
                && matches!(
                    &*attribute.value,
                    Expr::Name(name) if self.modules.iter().any(|module| module == name.id.as_str())
                ))
            .then_some(Loader::ImportModule),
            Expr::Name(name) => self
                .functions
                .iter()
                .find(|(bound, _)| bound == name.id.as_str())
                .map(|(_, loader)| *loader),
            _ => None,
        }
    }
}

/// What a loader call with a literal module name loads.
#[derive(Debug)]
pub(super) enum LiteralTarget {
    /// This absolute module.
    Module(String),
    /// A module relative to a package this walk cannot see.
    Opaque,
    /// Nothing: the call raises before importing (empty or malformed name,
    /// relative name without a package).
    Nothing,
}

/// Literal module name passed to a loader call, positionally or as `name=`,
/// made absolute the way `loader` resolves it.
///
/// `current_package` is the `__package__` of the calling module. Returns
/// `None` when the name is not a literal.
#[must_use]
pub(super) fn literal_target(
    call: &ExprCall,
    loader: Loader,
    current_package: impl FnOnce() -> Option<String>,
) -> Option<LiteralTarget> {
    let name = str_constant(module_argument(call)?)?;
    let module = match loader {
        Loader::ImportModule => import_module_name(call, name, current_package),
        Loader::DunderImport => dunder_import_name(call, name, current_package),
    };
    // `importlib` does not require identifiers: a `tests/my-harness/` directory
    // imports as `tests.my-harness`. Only an empty segment can never resolve.
    Some(match module {
        Ok(module) if module.split('.').all(|part| !part.is_empty()) => {
            LiteralTarget::Module(module)
        },
        Ok(_) => LiteralTarget::Nothing,
        Err(target) => target,
    })
}

/// `name` made absolute against `package=`, as `importlib.util.resolve_name` does.
fn import_module_name(
    call: &ExprCall,
    name: &str,
    current_package: impl FnOnce() -> Option<String>,
) -> Result<String, LiteralTarget> {
    if !name.starts_with('.') {
        return Ok(name.to_owned());
    }
    let package = match argument(call, 1, "package") {
        None | Some(Expr::NoneLiteral(_)) => return Err(LiteralTarget::Nothing),
        Some(Expr::Name(name)) if name.id.as_str() == "__package__" => current_package(),
        Some(expr) => str_constant(expr).map(str::to_owned),
    };
    let package = package.ok_or(LiteralTarget::Opaque)?;
    resolve_relative_name(name, &package).ok_or(LiteralTarget::Nothing)
}

/// `name` made absolute `level=` packages up from the package of `globals`,
/// as `importlib._bootstrap._resolve_name` does. A leading dot in `name` is
/// not relative here: it leaves an empty segment.
fn dunder_import_name(
    call: &ExprCall,
    name: &str,
    current_package: impl FnOnce() -> Option<String>,
) -> Result<String, LiteralTarget> {
    let level = match argument(call, 4, "level") {
        None => 0,
        Some(Expr::NumberLiteral(ExprNumberLiteral {
            value: Number::Int(level),
            ..
        })) => level.as_u8().ok_or(LiteralTarget::Nothing)?,
        Some(_) => return Err(LiteralTarget::Opaque),
    };
    if level == 0 {
        return Ok(name.to_owned());
    }
    let package = match argument(call, 1, "globals") {
        None | Some(Expr::NoneLiteral(_)) => return Err(LiteralTarget::Nothing),
        Some(Expr::Call(globals))
            if globals.arguments.is_empty()
                && matches!(&*globals.func, Expr::Name(name) if name.id.as_str() == "globals") =>
        {
            current_package()
        },
        Some(_) => None,
    };
    let package = package.ok_or(LiteralTarget::Opaque)?;
    let base =
        resolve_relative_name(&".".repeat(level.into()), &package).ok_or(LiteralTarget::Nothing)?;
    Ok(if name.is_empty() {
        base
    } else {
        format!("{base}.{name}")
    })
}

/// Package whose submodule a loader call builds from a literal prefix:
/// `"pkg.commands." + name` or `f"pkg.commands.{name}"` gives `pkg.commands`.
#[must_use]
pub(super) fn module_prefix(call: &ExprCall) -> Option<String> {
    let literal = match module_argument(call)? {
        Expr::BinOp(binop) if matches!(binop.op, Operator::Add) => leftmost_str(&binop.left)?,
        Expr::FString(fstring) if !fstring.value.is_implicit_concatenated() => {
            &*fstring.value.elements().next()?.as_literal()?.value
        },
        _ => return None,
    };
    let package = literal.strip_suffix('.')?;
    is_dotted_identifier(package).then(|| package.to_owned())
}

fn module_argument(call: &ExprCall) -> Option<&Expr> {
    argument(call, 0, "name")
}

pub(super) fn argument<'a>(call: &'a ExprCall, index: usize, keyword: &str) -> Option<&'a Expr> {
    call.arguments.args.get(index).or_else(|| {
        call.arguments
            .keywords
            .iter()
            .find(|candidate| {
                candidate
                    .arg
                    .as_ref()
                    .is_some_and(|arg| arg.as_str() == keyword)
            })
            .map(|candidate| &candidate.value)
    })
}

/// The literal a chain of `+` starts with (`"a." + b + ".c"` gives `a.`).
fn leftmost_str(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::BinOp(binop) if matches!(binop.op, Operator::Add) => leftmost_str(&binop.left),
        _ => str_constant(expr),
    }
}

/// Module names a `pytest_plugins = …` value makes pytest import: a string,
/// or the string elements of a list or tuple.
#[must_use]
pub(super) fn pytest_plugin_names(value: &Expr) -> Vec<(&str, &Expr)> {
    let elts: &[Expr] = match value {
        Expr::List(list) => &list.elts,
        Expr::Tuple(tuple) => &tuple.elts,
        _ => std::slice::from_ref(value),
    };
    elts.iter()
        .filter_map(|elt| str_constant(elt).map(|name| (name, elt)))
        .filter(|(name, _)| name.split('.').all(|part| !part.is_empty()))
        .collect()
}

/// What a `[sys.executable, …]` argument list runs in this interpreter's
/// environment.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PythonRun {
    /// `[sys.executable, "-m", "module", …]`.
    Module(String),
    /// `[sys.executable, <file>, …]` with a path the parser cannot follow.
    File,
}

/// Recognize a `subprocess` argument list starting with `sys.executable`.
#[must_use]
pub(super) fn python_run(elts: &[Expr]) -> Option<PythonRun> {
    let (first, rest) = elts.split_first()?;
    let Expr::Attribute(attribute) = first else {
        return None;
    };
    if attribute.attr.as_str() != "executable"
        || !matches!(&*attribute.value, Expr::Name(name) if name.id.as_str() == "sys")
    {
        return None;
    }
    match rest {
        [flag, module, ..] if str_constant(flag) == Some("-m") => str_constant(module)
            .filter(|module| is_dotted_identifier(module))
            .map(|module| PythonRun::Module(module.to_owned())),
        [argument, ..] if str_constant(argument).is_none_or(|value| !value.starts_with('-')) => {
            Some(PythonRun::File)
        },
        _ => None,
    }
}

/// The program in a command string or the literal head of an argv list/tuple.
#[must_use]
pub(super) fn command_word(expr: &Expr) -> Option<&str> {
    let first = match expr {
        Expr::List(list) => str_constant(list.elts.first()?)?,
        Expr::Tuple(tuple) => str_constant(tuple.elts.first()?)?,
        _ => {
            let mut words = str_constant(expr)?.split_whitespace();
            let first = words.next()?;
            words.next()?;
            first
        },
    };
    let valid = first.starts_with(|c: char| c.is_ascii_alphabetic())
        && first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    valid.then_some(first)
}

/// A string literal that is a whole dotted module name, which the code may
/// load by name (airflow's `lazy_load_command("pkg.cli.x.cmd")`, gunicorn's
/// `"python:pkg.config"`): identifier segments joined by `.`, two or more,
/// with an optional `python:` prefix (#1026), and an optional `:attr` after
/// them (celery's `"pkg.backends.redis:RedisBackend"`, #493). One segment
/// (`"json"`, `"main:app"`), a path (`"a/b.py"`) or a sentence is not one;
/// a computed string (an f-string or a concatenation with a name) is not a
/// literal, so a module loaded that way is not seen and its finding is not
/// lowered.
#[must_use]
pub(super) fn module_ref_literal(expr: &Expr) -> Option<&str> {
    let text = str_constant(expr)?;
    // The visitor checks every literal, docstrings included: reject one at
    // its first ASCII character no reference holds (a space, mostly) before
    // scanning it whole (#1087). A non-ASCII letter may be an identifier's.
    if text
        .bytes()
        .any(|b| b.is_ascii() && !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':')))
    {
        return None;
    }
    let (module, attr) = split_attr(text.strip_prefix("python:").unwrap_or(text));
    (module.contains('.') && is_dotted_identifier(module) && attr.is_none_or(is_dotted_identifier))
        .then_some(text)
}

/// The module a [`module_ref_literal`] names: without its `python:` prefix
/// and its `:attr`.
#[must_use]
pub(crate) fn module_ref_name(text: &str) -> &str {
    split_attr(text.strip_prefix("python:").unwrap_or(text)).0
}

/// `pkg.mod:attr` as the module and the attribute.
fn split_attr(name: &str) -> (&str, Option<&str>) {
    name.split_once(':')
        .map_or((name, None), |(module, attr)| (module, Some(attr)))
}

/// Whether a call adds a directory to `sys.path`: `sys.path.insert` /
/// `append` / `extend`, `site.addsitedir`, or pytest's
/// `monkeypatch.syspath_prepend`.
#[must_use]
pub(super) fn edits_sys_path(func: &Expr) -> bool {
    let Expr::Attribute(attribute) = func else {
        return false;
    };
    let receiver_is = |dotted: &str| is_receiver(&attribute.value, dotted);
    match attribute.attr.as_str() {
        "insert" | "append" | "extend" => receiver_is("sys.path"),
        "addsitedir" => receiver_is("site"),
        "syspath_prepend" => true,
        _ => false,
    }
}

/// Whether a call lists a directory or a package's modules, by the callee's
/// last name: `pkgutil`'s `iter_modules` / `walk_packages`, `glob` / `iglob`
/// and `pathlib`'s `glob` / `rglob` / `iterdir`, `os.listdir` / `scandir`,
/// and `os.walk` (#1267). `walk` needs the `os` receiver, since `ast.walk`
/// lists no directory.
#[must_use]
pub(super) fn lists_directory(func: &Expr) -> bool {
    let (name, receiver) = match func {
        Expr::Name(name) => (name.id.as_str(), None),
        Expr::Attribute(attribute) => (attribute.attr.as_str(), Some(&*attribute.value)),
        _ => return false,
    };
    match name {
        "iter_modules" | "walk_packages" | "glob" | "iglob" | "rglob" | "iterdir" | "listdir"
        | "scandir" => true,
        "walk" => receiver.is_some_and(|receiver| is_receiver(receiver, "os")),
        _ => false,
    }
}

/// A string literal that may name a directory added to `sys.path`: a
/// relative path, without its `.` and `..` segments (`"../exts"` gives
/// `exts`). `"."` gives `""`, the module's own directory.
#[must_use]
pub(super) fn sys_path_hint(expr: &Expr) -> Option<Cow<'_, str>> {
    // The visitor checks every literal, so most are rejected before any copy.
    let value = str_constant(expr)?;
    let separator = |c: char| matches!(c, '/' | '\\');
    let path_like = !value.starts_with(separator)
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\'));
    if !path_like {
        return None;
    }
    if value.trim_end_matches(separator) == "." {
        return Some(Cow::Borrowed(""));
    }
    let skipped = |segment: &str| matches!(segment, "" | "." | "..");
    if !value.contains('\\') && !value.split('/').any(skipped) {
        return Some(Cow::Borrowed(value));
    }
    let segments: Vec<&str> = value
        .split(separator)
        .filter(|segment| !skipped(segment))
        .collect();
    (!segments.is_empty()).then(|| Cow::Owned(segments.join("/")))
}

pub(super) fn str_constant(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::StringLiteral(literal) => Some(literal.value.to_str()),
        _ => None,
    }
}

/// A call that may load the name its first argument spells (#493): the
/// callee's last name, the name with `:` written as `.`, and whether it was
/// written `pkg.mod:attr`, the shape of a cyclopts lazy command. The name is
/// a dotted name of two or more segments (`"pkg.mod.Cls"`) or a dotted
/// module and attribute (`"pkg:Cls"`, `"pkg.mod:Cls.attr"`). A one-segment
/// name, a computed string, a name passed by keyword and a callee that is
/// not a name (`loaders[0](...)`) are not read, so a file only such a call
/// loads is reported unused.
#[must_use]
pub(super) fn named_load(call: &ExprCall) -> Option<(&str, String, bool)> {
    let callee = match &*call.func {
        Expr::Name(name) => name.id.as_str(),
        Expr::Attribute(attribute) => attribute.attr.as_str(),
        _ => return None,
    };
    let text = str_constant(call.arguments.args.first()?)?;
    match text.split_once(':') {
        Some((module, attr)) => (is_dotted_identifier(module) && is_dotted_identifier(attr))
            .then(|| (callee, format!("{module}.{attr}"), module.contains('.'))),
        None => (text.contains('.') && is_dotted_identifier(text))
            .then(|| (callee, text.to_owned(), false)),
    }
}

/// Loader APIs that import the module a dotted name starts with and fetch
/// the rest as attributes (#493): Django's and werkzeug's `import_string`,
/// celery's `symbol_by_name`, DRF's `import_from_string`, scrapy's
/// `load_object`. They are matched by the callee's last name, so a
/// project's own helper of the same name (airflow's `import_string`) counts
/// as well. `pkgutil.resolve_name` is left to `module_loaders`: the name is
/// as common for resolving a host name (`resolve_name("google.com")`).
const MODULE_LOADERS: [&str; 4] = [
    "import_string",
    "symbol_by_name",
    "import_from_string",
    "load_object",
];

/// Flask's `Config.from_object`, whose module the Flask plugin reads the
/// upper-case names of as settings (#1238). It is no loader, as a method of
/// the same name elsewhere would make the name an import edge.
const CONFIG_OBJECT_CALL: &str = "from_object";

/// Moves each named load whose callee is a loader into its module's dynamic
/// imports, and drops the rest (#493).
///
/// The loaders are `MODULE_LOADERS`, cyclopts' lazy `command("pkg.mod:app")`
/// (a dotted `module:attr` only, as a click or typer command name may be
/// `db:migrate`) and the names `module_loaders` configures. A `from_object`
/// load is also kept as a config object (#1238).
pub fn apply_module_loaders(modules: &mut [ParsedModule], configured: &[String]) {
    for module in modules {
        for named in std::mem::take(&mut module.named_loads) {
            let call = named.call.as_str();
            if call == CONFIG_OBJECT_CALL {
                module.config_objects.push(named.load.clone());
            }
            if MODULE_LOADERS.contains(&call)
                || (named.lazy_command && call == "command")
                || configured.iter().any(|name| name == call)
            {
                module.dynamic_imports.push(named.load);
            }
        }
    }
}

pub(super) fn is_dotted_identifier(module: &str) -> bool {
    module.split('.').all(|part| {
        part.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && part.chars().all(|c| c.is_alphanumeric() || c == '_')
    })
}

#[cfg(test)]
mod tests {
    use ruff_python_ast::Expr;

    use super::super::types::{DynamicImport, NamedLoad, ParsedModule};
    use super::{
        LiteralTarget, Loader, LoaderNames, PythonRun, apply_module_loaders, command_word,
        edits_sys_path, literal_target, module_prefix, module_ref_literal, module_ref_name,
        named_load, python_run, sys_path_hint,
    };

    fn expr(source: &str) -> Expr {
        ruff_python_parser::parse_expression(source)
            .expect("parse")
            .expr()
            .clone()
    }

    fn run(source: &str) -> Option<PythonRun> {
        match expr(source) {
            Expr::List(list) => python_run(&list.elts),
            Expr::Tuple(tuple) => python_run(&tuple.elts),
            _ => None,
        }
    }

    fn prefix(source: &str) -> Option<String> {
        let Expr::Call(call) = expr(source) else {
            panic!("expected a call");
        };
        module_prefix(&call)
    }

    fn target(source: &str, loader: Loader) -> Option<LiteralTarget> {
        let Expr::Call(call) = expr(source) else {
            panic!("expected a call");
        };
        literal_target(&call, loader, || Some("acme.core".to_owned()))
    }

    /// A whole dotted name, with or without `python:`, is a module reference,
    /// and so is `pkg.mod:attr` (#493); one segment, a one-segment module
    /// before `:`, a path, a sentence or a computed string is not (#1026).
    /// A loader call names a dotted name or a `module:attr` in its first
    /// positional argument; one segment, a keyword or a computed string is
    /// not read, nor is a callee without a name (#493).
    #[test]
    fn named_loads_read_the_first_literal_dotted_name() {
        let load = |source: &str| {
            let Expr::Call(call) = expr(source) else {
                panic!("not a call: {source}");
            };
            named_load(&call).map(|(callee, name, lazy)| (callee.to_owned(), name, lazy))
        };
        let read =
            |callee: &str, name: &str, lazy: bool| Some((callee.to_owned(), name.to_owned(), lazy));
        assert_eq!(
            load(r#"import_string("pkg.mod.Cls")"#),
            read("import_string", "pkg.mod.Cls", false)
        );
        assert_eq!(
            load(r#"kombu.utils.symbol_by_name("pkg.mod:Cls.attr", default=None)"#),
            read("symbol_by_name", "pkg.mod.Cls.attr", true)
        );
        assert_eq!(
            load(r#"command("db:migrate")"#),
            read("command", "db.migrate", false)
        );
        for source in [
            r#"import_string("pkg")"#,
            r#"import_string(":Cls")"#,
            r#"import_string("pkg.mod:")"#,
            r#"import_string("pkg:a:b")"#,
            r#"import_string("pkg/mod.py")"#,
            r#"import_string(f"pkg.{name}")"#,
            r#"import_string(name, "pkg.mod")"#,
            r#"import_string(dotted_path="pkg.mod")"#,
            r#"loaders[0]("pkg.mod")"#,
        ] {
            assert_eq!(load(source), None, "{source}");
        }
    }

    /// Built-in loaders, configured names and a dotted lazy `command` are
    /// kept; any other callee, and `command` given `db:migrate`, is dropped.
    #[test]
    fn module_loaders_keep_only_loader_calls() {
        let named = |call: &str, lazy_command: bool| NamedLoad {
            call: call.to_owned(),
            lazy_command,
            load: DynamicImport {
                module: format!("{call}.{lazy_command}"),
                ..DynamicImport::default()
            },
        };
        let mut modules = [ParsedModule {
            named_loads: vec![
                named("import_string", false),
                named("LazyFeature", false),
                named("command", true),
                named("command", false),
                named("helper", true),
                named("from_object", false),
            ],
            ..ParsedModule::default()
        }];
        apply_module_loaders(&mut modules, &["LazyFeature".to_owned()]);
        let kept: Vec<_> = modules[0]
            .dynamic_imports
            .iter()
            .map(|load| load.module.as_str())
            .collect();
        assert_eq!(
            kept,
            ["import_string.false", "LazyFeature.false", "command.true"]
        );
        let objects: Vec<_> = modules[0]
            .config_objects
            .iter()
            .map(|load| load.module.as_str())
            .collect();
        assert_eq!(objects, ["from_object.false"]);
        assert!(modules[0].named_loads.is_empty());
    }

    #[test]
    fn module_ref_literals_are_whole_dotted_names() {
        let module_ref = |source: &str| module_ref_literal(&expr(source)).map(str::to_owned);
        assert_eq!(
            module_ref(r#""pkg.cli.cheat_sheet.cheat_sheet""#),
            Some("pkg.cli.cheat_sheet.cheat_sheet".to_owned())
        );
        assert_eq!(
            module_ref(r#""python:pkg.gunicorn_config""#),
            Some("python:pkg.gunicorn_config".to_owned())
        );
        assert_eq!(
            module_ref_name("python:pkg.gunicorn_config"),
            "pkg.gunicorn_config"
        );
        assert_eq!(
            module_ref(r#""paket.größe""#),
            Some("paket.größe".to_owned())
        );
        assert_eq!(
            module_ref(r#""_private.mod2""#),
            Some("_private.mod2".to_owned())
        );
        assert_eq!(
            module_ref(r#""pkg.backends.redis:RedisBackend""#),
            Some("pkg.backends.redis:RedisBackend".to_owned())
        );
        assert_eq!(
            module_ref_name("pkg.backends.redis:RedisBackend"),
            "pkg.backends.redis"
        );
        assert_eq!(module_ref_name("python:pkg.app:make.app"), "pkg.app");
        for source in [
            r#""json""#,
            r#""pkg:attr""#,
            r#""pkg.mod:""#,
            r#""pkg.mod:attr:more""#,
            r#""pkg.mod:1attr""#,
            r#""localhost:8080""#,
            r#""pkg/mod.py""#,
            r#""pkg.mod.""#,
            r#"".pkg.mod""#,
            r#""pkg..mod""#,
            r#""1pkg.mod""#,
            r#""pkg.mod is loaded.""#,
            r#""python:""#,
            r#"f"pkg.{name}""#,
            r#""pkg." + name"#,
            r#"b"pkg.mod""#,
        ] {
            assert_eq!(module_ref(source), None, "{source}");
        }
    }

    #[test]
    fn recognizes_only_importlib_import_module_attributes() {
        let names = LoaderNames::default();
        assert!(matches!(
            names.loader(&expr("importlib.import_module")),
            Some(Loader::ImportModule)
        ));
        assert!(names.loader(&expr("importlib.reload")).is_none());
        assert!(names.loader(&expr("other.import_module")).is_none());
    }

    #[test]
    fn reads_the_caller_package_only_from_its_own_name() {
        assert!(matches!(
            target(r#"import_module(".sub", __package__)"#, Loader::ImportModule),
            Some(LiteralTarget::Module(module)) if module == "acme.core.sub"
        ));
        assert!(matches!(
            target(r#"import_module(".sub", base)"#, Loader::ImportModule),
            Some(LiteralTarget::Opaque)
        ));
        assert!(matches!(
            target(r#"__import__("sub", globals(), None, (), 1)"#, Loader::DunderImport),
            Some(LiteralTarget::Module(module)) if module == "acme.core.sub"
        ));
        for globals in ["vars()", "globals(other)", "namespace"] {
            let source = format!(r#"__import__("sub", {globals}, None, (), 1)"#);
            assert!(
                matches!(
                    target(&source, Loader::DunderImport),
                    Some(LiteralTarget::Opaque)
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn recognizes_calls_that_edit_sys_path() {
        for func in [
            "sys.path.insert",
            "sys.path.append",
            "sys.path.extend",
            "site.addsitedir",
            "monkeypatch.syspath_prepend",
            "self.monkeypatch.syspath_prepend",
        ] {
            assert!(edits_sys_path(&expr(func)), "{func}");
        }
        for func in [
            "path.insert",
            "sys.modules.insert",
            "items.append",
            "addsitedir",
        ] {
            assert!(!edits_sys_path(&expr(func)), "{func}");
        }
    }

    #[test]
    fn sys_path_hints_are_relative_directory_literals() {
        let hint = |source: &str| sys_path_hint(&expr(source)).map(std::borrow::Cow::into_owned);
        assert_eq!(hint(r#""utils""#).as_deref(), Some("utils"));
        assert_eq!(hint(r#""docs/_ext""#).as_deref(), Some("docs/_ext"));
        assert_eq!(hint(r#""../exts""#).as_deref(), Some("exts"));
        assert_eq!(hint(r#""./test_apps/""#).as_deref(), Some("test_apps"));
        assert_eq!(hint(r#""docs//_ext""#).as_deref(), Some("docs/_ext"));
        assert_eq!(hint(r#""..\\dev""#).as_deref(), Some("dev"));
        assert_eq!(hint(r#""tools\\lib\\""#).as_deref(), Some("tools/lib"));
        assert_eq!(hint(r#"".""#).as_deref(), Some(""));
        assert_eq!(hint(r#"".\\""#).as_deref(), Some(""));
        for source in [
            r#""..""#,
            r#""""#,
            r#""/opt/lib""#,
            r#""\\opt\\lib""#,
            r#""C:/lib""#,
            r#""a b""#,
            r#"f"{root}/utils""#,
            "name",
        ] {
            assert_eq!(hint(source), None, "{source}");
        }
    }

    #[test]
    fn recognizes_prefixed_module_names() {
        assert_eq!(
            prefix(r#"import_module("pkg.commands." + ".".join(words))"#),
            Some("pkg.commands".to_owned())
        );
        assert_eq!(
            prefix(r#"import_module("pkg." + name + ".impl")"#),
            Some("pkg".to_owned())
        );
        assert_eq!(
            prefix(r#"import_module(f"pkg.plugins.{name}")"#),
            Some("pkg.plugins".to_owned())
        );
        assert_eq!(prefix(r#"import_module("pkg" + name)"#), None);
        assert_eq!(prefix(r#"import_module(f"{base}.plugins")"#), None);
        assert_eq!(prefix("import_module(name)"), None);
    }

    #[test]
    fn takes_the_program_of_command_lines() {
        let word = |source: &str| command_word(&expr(source)).map(str::to_owned);
        assert_eq!(
            word(r#""ruff format --force-exclude 2>&1""#),
            Some("ruff".to_owned())
        );
        assert_eq!(word(r#""pre-commit run""#), Some("pre-commit".to_owned()));
        assert_eq!(word(r#""ruff""#), None);
        assert_eq!(word(r#""./tool.sh run""#), None);
        assert_eq!(word(r#""`ruff` failed""#), None);
        assert_eq!(word("name"), None);
    }

    #[test]
    fn recognizes_module_and_file_runs() {
        assert_eq!(
            run(r#"[sys.executable, "-m", "virtualenv", path]"#),
            Some(PythonRun::Module("virtualenv".to_owned()))
        );
        assert_eq!(
            run("(sys.executable, script.as_posix())"),
            Some(PythonRun::File)
        );
        assert_eq!(run(r#"[sys.executable, "tool.py"]"#), Some(PythonRun::File));
    }

    #[test]
    fn ignores_other_argument_lists() {
        assert_eq!(run(r#"[sys.executable, "-c", code]"#), None);
        assert_eq!(run(r#"[sys.executable, "-m", name]"#), None);
        assert_eq!(run("[sys.executable]"), None);
        assert_eq!(run(r#"["python", "-m", "pip"]"#), None);
    }

    mod props {
        use proptest::prelude::*;
        use ruff_python_ast::Expr;

        use super::expr;
        use crate::parser::dynamic::{LiteralTarget, Loader, literal_target};

        #[derive(Debug, Clone)]
        enum Package {
            Absent,
            NoneLiteral,
            Literal(String),
            /// `package=__package__`, with the caller's `__package__`.
            Dunder(Option<String>),
        }

        #[derive(Debug, PartialEq, Eq)]
        enum Outcome {
            Module(String),
            Opaque,
            Nothing,
        }

        /// `importlib.import_module(name, package)` per `CPython` 3.11,
        /// written from `importlib/__init__.py` and `_bootstrap._resolve_name`.
        fn import_module(name: &str, package: &Package) -> Outcome {
            let valid = |module: String| {
                if module.split('.').any(str::is_empty) {
                    Outcome::Nothing
                } else {
                    Outcome::Module(module)
                }
            };
            let Some(relative) = name.strip_prefix('.') else {
                return valid(name.to_owned());
            };
            let package = match package {
                Package::Absent | Package::NoneLiteral => return Outcome::Nothing,
                Package::Dunder(None) => return Outcome::Opaque,
                Package::Literal(package) | Package::Dunder(Some(package)) => package,
            };
            // `if not package: raise TypeError`.
            if package.is_empty() {
                return Outcome::Nothing;
            }
            let rest = relative.trim_start_matches('.');
            let level = name.len() - rest.len();
            // `bits = package.rsplit('.', level - 1); if len(bits) < level: raise`.
            let bits: Vec<&str> = package.rsplitn(level, '.').collect();
            if bits.len() < level {
                return Outcome::Nothing;
            }
            let base = bits.last().copied().unwrap_or_default();
            valid(if rest.is_empty() {
                base.to_owned()
            } else {
                format!("{base}.{rest}")
            })
        }

        fn dotted() -> impl Strategy<Value = String> {
            "[ab.]{0,7}"
        }

        fn package() -> impl Strategy<Value = Package> {
            prop_oneof![
                Just(Package::Absent),
                Just(Package::NoneLiteral),
                dotted().prop_map(Package::Literal),
                prop::option::of(dotted()).prop_map(Package::Dunder),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(crate::internals::proptest_cases(256)))]

            #[test]
            fn literal_target_matches_import_module(
                name in dotted(),
                package in package(),
                keyword in any::<bool>(),
            ) {
                let argument = match &package {
                    Package::Absent => String::new(),
                    Package::NoneLiteral => "None".to_owned(),
                    Package::Literal(package) => format!("{package:?}"),
                    Package::Dunder(_) => "__package__".to_owned(),
                };
                let call = match (argument.is_empty(), keyword) {
                    (true, _) => format!("importlib.import_module({name:?})"),
                    (false, false) => format!("importlib.import_module({name:?}, {argument})"),
                    (false, true) => format!("importlib.import_module(name={name:?}, package={argument})"),
                };
                let Expr::Call(call) = expr(&call) else {
                    return Err(TestCaseError::fail("expected a call"));
                };
                let current = match &package {
                    Package::Dunder(current) => current.clone(),
                    _ => None,
                };
                let got = match literal_target(&call, Loader::ImportModule, || current) {
                    Some(LiteralTarget::Module(module)) => Outcome::Module(module),
                    Some(LiteralTarget::Opaque) => Outcome::Opaque,
                    Some(LiteralTarget::Nothing) => Outcome::Nothing,
                    None => return Err(TestCaseError::fail("literal name not recognised")),
                };
                prop_assert_eq!(got, import_module(&name, &package));
            }
        }
    }
}
