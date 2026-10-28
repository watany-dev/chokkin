//! Apps handed to uvicorn in Python: `uvicorn.run("pkg.asgi:app")` and
//! `uvicorn.Config("pkg.asgi:app")` (#1004).

use ruff_python_ast::{Alias, Expr, ExprCall};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use super::dynamic::{argument, str_constant};

/// Names one module binds to uvicorn's app loaders.
#[derive(Default)]
pub(super) struct ServerNames {
    /// Names bound to the `uvicorn` package.
    modules: HashSet<String>,
    /// Names bound directly to `run` or `Config`, with the call's label.
    functions: HashMap<String, &'static str>,
}

impl ServerNames {
    /// Track `import uvicorn [as u]`; `import uvicorn.config` binds `uvicorn`.
    pub(super) fn record_import(&mut self, alias: &Alias) {
        let name = alias.name.as_str();
        match &alias.asname {
            Some(asname) if name == "uvicorn" => {
                self.modules.insert(asname.to_string());
            },
            None if name == "uvicorn" || name.starts_with("uvicorn.") => {
                self.modules.insert("uvicorn".to_owned());
            },
            _ => {},
        }
    }

    /// Track `from uvicorn import run, Config [as c]`, and the submodules that
    /// define them.
    pub(super) fn record_import_from(&mut self, module: Option<&str>, alias: &Alias) {
        let label = match (module, alias.name.as_str()) {
            (Some("uvicorn" | "uvicorn.main"), "run") => "uvicorn.run",
            (Some("uvicorn" | "uvicorn.config"), "Config") => "uvicorn.Config",
            _ => return,
        };
        let bound = alias.asname.as_ref().unwrap_or(&alias.name);
        self.functions.insert(bound.to_string(), label);
    }

    /// The literal `module:attr` app a call to `run` or `Config` loads, given
    /// positionally or as `app=`, with the call's label and whether `app_dir=`
    /// moves its import root. An app object or a computed string is not read:
    /// the object's module is imported where it is named, and the string is a
    /// recall gap.
    pub(super) fn app<'a>(&self, call: &'a ExprCall) -> Option<(&'static str, &'a str, bool)> {
        let label = match &*call.func {
            Expr::Attribute(attribute) => match &*attribute.value {
                Expr::Name(name) if self.modules.contains(name.id.as_str()) => {
                    match attribute.attr.as_str() {
                        "run" => "uvicorn.run",
                        "Config" => "uvicorn.Config",
                        _ => return None,
                    }
                },
                _ => return None,
            },
            Expr::Name(name) => self.functions.get(name.id.as_str()).copied()?,
            _ => return None,
        };
        // uvicorn rejects an import string without `:`.
        let app = str_constant(argument(call, 0, "app")?)?;
        let app_dir = call.arguments.find_keyword("app_dir").is_some();
        app.contains(':').then_some((label, app, app_dir))
    }
}
