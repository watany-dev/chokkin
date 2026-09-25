//! `importlib.import_module` and `__import__` literal recognition.

use std::collections::HashSet;

use ruff_python_ast::{Alias, Expr, ExprCall};

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
    let module = call.arguments.args.first().or_else(|| {
        call.arguments
            .keywords
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
        Expr::StringLiteral(literal) => Some(literal.value.to_str().to_owned()),
        _ => None,
    }
}
