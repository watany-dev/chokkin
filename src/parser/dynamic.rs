//! `importlib.import_module` and `__import__` literal recognition.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use ruff_python_ast::{Alias, Expr, ExprCall, ExprNumberLiteral, Number, Operator};

use super::attributes::attribute_receiver;
use super::relative::resolve_relative_name;

#[derive(Debug, Clone, Copy)]
pub(super) enum Loader {
    /// `importlib.import_module(name, package=None)`.
    ImportModule,
    /// `__import__(name, globals=None, locals=None, fromlist=(), level=0)`.
    DunderImport,
}

/// Names one module binds to the dynamic import loaders.
pub(super) struct LoaderNames {
    /// Names bound to the `importlib` module.
    modules: HashSet<String>,
    /// Names bound directly to a loader function.
    functions: HashMap<String, Loader>,
}

impl Default for LoaderNames {
    fn default() -> Self {
        Self {
            modules: HashSet::from(["importlib".to_owned()]),
            functions: HashMap::from([("__import__".to_owned(), Loader::DunderImport)]),
        }
    }
}

impl LoaderNames {
    /// Track `import importlib as il`.
    pub(super) fn record_import(&mut self, alias: &Alias) {
        if alias.name.as_str() == "importlib"
            && let Some(asname) = &alias.asname
        {
            self.modules.insert(asname.to_string());
        }
    }

    /// Track `from importlib import import_module [as im]`.
    pub(super) fn record_import_from(&mut self, module: Option<&str>, alias: &Alias) {
        if module == Some("importlib") && alias.name.as_str() == "import_module" {
            let bound = alias.asname.as_ref().unwrap_or(&alias.name);
            self.functions
                .insert(bound.to_string(), Loader::ImportModule);
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
                    Expr::Name(name) if self.modules.contains(name.id.as_str())
                ))
            .then_some(Loader::ImportModule),
            Expr::Name(name) => self.functions.get(name.id.as_str()).copied(),
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

fn argument<'a>(call: &'a ExprCall, index: usize, keyword: &str) -> Option<&'a Expr> {
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

/// The program a shell command line would run: the first word of a string
/// literal that has more than one (`"ruff format --check"` gives `ruff`).
#[must_use]
pub(super) fn command_word(expr: &Expr) -> Option<&str> {
    let mut words = str_constant(expr)?.split_whitespace();
    let first = words.next()?;
    words.next()?;
    let valid = first.starts_with(|c: char| c.is_ascii_alphabetic())
        && first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    valid.then_some(first)
}

/// Whether a call adds a directory to `sys.path`: `sys.path.insert` /
/// `append` / `extend`, `site.addsitedir`, or pytest's
/// `monkeypatch.syspath_prepend`.
#[must_use]
pub(super) fn edits_sys_path(func: &Expr) -> bool {
    let Expr::Attribute(attribute) = func else {
        return false;
    };
    let receiver_is = |dotted: &str| {
        attribute_receiver(&attribute.value).is_some_and(|receiver| receiver == dotted)
    };
    match attribute.attr.as_str() {
        "insert" | "append" | "extend" => receiver_is("sys.path"),
        "addsitedir" => receiver_is("site"),
        "syspath_prepend" => true,
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

fn str_constant(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::StringLiteral(literal) => Some(literal.value.to_str()),
        _ => None,
    }
}

fn is_dotted_identifier(module: &str) -> bool {
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

    use super::{
        LiteralTarget, Loader, LoaderNames, PythonRun, command_word, edits_sys_path,
        literal_target, module_prefix, python_run, sys_path_hint,
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
            #![proptest_config(ProptestConfig::with_cases(256))]

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
