//! `importlib.import_module` and `__import__` literal recognition.

use std::collections::HashSet;

use rustpython_parser::ast::{Alias, Constant, Expr, ExprCall};

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

/// Literal module name passed to a loader call, positionally or as `name=`.
#[must_use]
pub fn literal_module(call: &ExprCall) -> Option<String> {
    let module = call.args.first().or_else(|| {
        call.keywords
            .iter()
            .find(|keyword| {
                keyword
                    .arg
                    .as_ref()
                    .is_some_and(|arg| arg.as_str() == "name")
            })
            .map(|keyword| &keyword.value)
    })?;
    match module {
        Expr::Constant(constant) => match &constant.value {
            Constant::Str(value) => Some(value.clone()),
            _ => None,
        },
        _ => None,
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

fn str_constant(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Constant(constant) => match &constant.value {
            Constant::Str(value) => Some(value.as_str()),
            _ => None,
        },
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
    use rustpython_parser::Parse;
    use rustpython_parser::ast::{Expr, Stmt, Suite};

    use super::{PythonRun, python_run};

    fn run(source: &str) -> Option<PythonRun> {
        let stmts = Suite::parse(source, "<test>").expect("parse");
        let Some(Stmt::Expr(stmt)) = stmts.first() else {
            return None;
        };
        match &*stmt.value {
            Expr::List(list) => python_run(&list.elts),
            Expr::Tuple(tuple) => python_run(&tuple.elts),
            _ => None,
        }
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
        assert_eq!(run(r#"[sys.executable]"#), None);
        assert_eq!(run(r#"["python", "-m", "pip"]"#), None);
    }
}
