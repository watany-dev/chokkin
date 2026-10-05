//! `importlib.import_module` and `__import__` literal recognition.

use std::collections::HashSet;

use ruff_python_ast::{Alias, Expr, ExprCall, Operator};

use super::relative::resolve_relative_name;

/// Names one module binds to the dynamic import loaders.
pub struct LoaderNames {
    /// Names bound to the `importlib` module.
    modules: HashSet<String>,
    /// Names bound directly to a loader function.
    functions: HashSet<String>,
}

impl Default for LoaderNames {
    fn default() -> Self {
        Self {
            modules: HashSet::from(["importlib".to_owned()]),
            functions: HashSet::from(["__import__".to_owned()]),
        }
    }
}

impl LoaderNames {
    /// Track `import importlib as il`.
    pub fn record_import(&mut self, alias: &Alias) {
        if alias.name.as_str() == "importlib"
            && let Some(asname) = &alias.asname
        {
            self.modules.insert(asname.to_string());
        }
    }

    /// Track `from importlib import import_module [as im]`.
    pub fn record_import_from(&mut self, module: Option<&str>, alias: &Alias) {
        if module == Some("importlib") && alias.name.as_str() == "import_module" {
            let bound = alias.asname.as_ref().unwrap_or(&alias.name);
            self.functions.insert(bound.to_string());
        }
    }

    /// Whether `expr` names `importlib.import_module` or `__import__`, aliases included.
    #[must_use]
    pub fn is_loader(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Attribute(attribute) => {
                attribute.attr.as_str() == "import_module"
                    && matches!(
                        &*attribute.value,
                        Expr::Name(name) if self.modules.contains(name.id.as_str())
                    )
            },
            Expr::Name(name) => self.functions.contains(name.id.as_str()),
            _ => false,
        }
    }
}

/// What a loader call with a literal module name loads.
#[derive(Debug)]
pub enum LiteralTarget {
    /// This absolute module.
    Module(String),
    /// A module relative to a package this walk cannot see.
    Opaque,
    /// Nothing: the call raises before importing (empty or malformed name,
    /// relative name without a package).
    Nothing,
}

/// Literal module name passed to a loader call, positionally or as `name=`,
/// made absolute against `package=` the way `importlib.util.resolve_name` does.
///
/// `current_package` is the `__package__` of the calling module. Returns
/// `None` when the name is not a literal.
#[must_use]
pub fn literal_target(
    call: &ExprCall,
    current_package: impl FnOnce() -> Option<String>,
) -> Option<LiteralTarget> {
    let name = str_constant(module_argument(call)?)?;
    let module = if name.starts_with('.') {
        let package = match argument(call, 1, "package") {
            None | Some(Expr::NoneLiteral(_)) => return Some(LiteralTarget::Nothing),
            Some(Expr::Name(name)) if name.id.as_str() == "__package__" => current_package(),
            Some(expr) => str_constant(expr).map(str::to_owned),
        };
        let Some(package) = package else {
            return Some(LiteralTarget::Opaque);
        };
        match resolve_relative_name(name, &package) {
            Some(module) => module,
            None => return Some(LiteralTarget::Nothing),
        }
    } else {
        name.to_owned()
    };
    Some(if is_dotted_identifier(&module) {
        LiteralTarget::Module(module)
    } else {
        LiteralTarget::Nothing
    })
}

/// Package whose submodule a loader call builds from a literal prefix:
/// `"pkg.commands." + name` or `f"pkg.commands.{name}"` gives `pkg.commands`.
#[must_use]
pub fn module_prefix(call: &ExprCall) -> Option<String> {
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

/// What a `[sys.executable, …]` argument list runs in this interpreter's
/// environment.
#[derive(Debug, PartialEq, Eq)]
pub enum PythonRun {
    /// `[sys.executable, "-m", "module", …]`.
    Module(String),
    /// `[sys.executable, <file>, …]` with a path the parser cannot follow.
    File,
}

/// Recognize a `subprocess` argument list starting with `sys.executable`.
#[must_use]
pub fn python_run(elts: &[Expr]) -> Option<PythonRun> {
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
pub fn command_word(expr: &Expr) -> Option<&str> {
    let mut words = str_constant(expr)?.split_whitespace();
    let first = words.next()?;
    words.next()?;
    let valid = first.starts_with(|c: char| c.is_ascii_alphabetic())
        && first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    valid.then_some(first)
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

    use super::{PythonRun, command_word, module_prefix, python_run};

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
}
