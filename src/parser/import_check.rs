//! Imports made after a call to a first-party install check (#1269).
//!
//! A project may check an optional package in a helper of its own before
//! importing it: the helper tries the import and raises from its `except
//! ImportError:` handler, so the code after the call runs only with the
//! package. The parser records the helpers each module defines
//! ([`ParsedModule::import_checks`]) and the calls each import runs after
//! ([`ImportRef::called_before`]); only the whole project tells which call
//! reaches which helper.

use std::collections::HashMap;

use crate::sources::{LayoutInfo, path_to_module};

use super::types::{ImportKind, ImportRef, ParsedModule};

/// Re-exports followed from a callee to the helper's definition; a chain
/// longer than this is a cycle or too far to trust.
const MAX_HOPS: usize = 8;

/// Mark each import optional that runs after a call reaching a first-party
/// function which checks the imported package is installed.
///
/// A callee is read through the calling module's imports (`from .utils
/// import check`, `import pkg.utils as u; u.check(...)`) or as a function of
/// the module itself, then through the re-exports of the modules it names,
/// such as a package `__init__.py`. An import of a checked module, of a
/// submodule of one (`x.pyplot` after checking `x`) or of a package a checked
/// module sits in is optional. A callee that reaches nothing leaves the
/// import as it is.
pub(crate) fn apply_import_checks(modules: &mut [ParsedModule], layout: &LayoutInfo) {
    if modules.iter().all(|module| module.import_checks.is_empty()) {
        return;
    }
    let names: Vec<Option<String>> = modules
        .iter()
        .map(|module| path_to_module(&module.path, layout))
        .collect();
    let index = ProjectIndex::new(modules, &names);
    let marks: Vec<Vec<usize>> = modules
        .iter()
        .zip(&names)
        .map(|(module, name)| {
            module
                .imports
                .iter()
                .enumerate()
                .filter(|(_, import)| {
                    !import.called_before.is_empty()
                        && import.called_before.iter().any(|callee| {
                            index
                                .checked_modules(module, name.as_deref(), callee)
                                .is_some_and(|checked| checks_import(checked, &import.module))
                        })
                })
                .map(|(position, _)| position)
                .collect()
        })
        .collect();
    for (module, marked) in modules.iter_mut().zip(marks) {
        for position in marked {
            module.imports[position].optional = true;
        }
    }
}

/// Whether checking `checked` proves `module` importable: the same module,
/// one inside it, or a package that holds it.
fn checks_import(checked: &[String], module: &str) -> bool {
    !module.is_empty()
        && checked
            .iter()
            .any(|checked| within(module, checked) || within(checked, module))
}

/// Whether `module` is `package` or a module inside it.
fn within(module: &str, package: &str) -> bool {
    module
        .strip_prefix(package)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// The project's install checks and module-level bindings, by module name.
struct ProjectIndex<'a> {
    /// `pkg.mod.function` to the modules the function checks.
    checks: HashMap<String, &'a [String]>,
    /// Module name to the module, for following its re-exports.
    modules: HashMap<&'a str, &'a ParsedModule>,
}

impl<'a> ProjectIndex<'a> {
    fn new(modules: &'a [ParsedModule], names: &'a [Option<String>]) -> Self {
        let mut checks = HashMap::new();
        let mut by_name = HashMap::new();
        for (module, name) in modules.iter().zip(names) {
            let Some(name) = name else {
                continue;
            };
            by_name.insert(name.as_str(), module);
            for check in &module.import_checks {
                checks.insert(
                    format!("{name}.{}", check.function),
                    check.modules.as_slice(),
                );
            }
        }
        Self {
            checks,
            modules: by_name,
        }
    }

    /// The modules the function `callee` names in `module` checks, if it is
    /// an install check.
    fn checked_modules(
        &self,
        module: &ParsedModule,
        name: Option<&str>,
        callee: &str,
    ) -> Option<&'a [String]> {
        let (head, rest) = match callee.split_once('.') {
            Some((head, rest)) => (head, Some(rest)),
            None => (callee, None),
        };
        let base = match binding_target(&module.imports, head) {
            Some(target) => target,
            None => format!("{}.{head}", name?),
        };
        let mut qualified = match rest {
            Some(rest) => format!("{base}.{rest}"),
            None => base,
        };
        for _ in 0..MAX_HOPS {
            if let Some(checked) = self.checks.get(&qualified) {
                return Some(checked);
            }
            let (owner, function) = qualified.rsplit_once('.')?;
            let owner = self.modules.get(owner)?;
            qualified = binding_target(&owner.imports, function)?;
        }
        None
    }
}

/// What the name `bound` refers to when one of `imports` binds it, as an
/// absolute dotted name: `from pkg.utils import check` binds `check` to
/// `pkg.utils.check`, `import pkg.utils as u` binds `u` to `pkg.utils`.
fn binding_target(imports: &[ImportRef], bound: &str) -> Option<String> {
    imports
        .iter()
        .find(|import| !import.module.is_empty() && import.bound_name() == Some(bound))
        .map(|import| match import.kind {
            ImportKind::Import => match &import.alias {
                Some(_) => import.module.clone(),
                None => bound.to_owned(),
            },
            ImportKind::ImportFrom => match &import.name {
                Some(name) => format!("{}.{name}", import.module),
                None => import.module.clone(),
            },
        })
}

#[cfg(test)]
mod tests {
    use crate::sources::{FileContext, LayoutInfo};

    use super::super::lines::LineIndex;
    use super::super::visit::ModuleVisitor;
    use super::*;

    fn parse(files: &[(&str, &str)]) -> Vec<ParsedModule> {
        let layout = LayoutInfo::new("src", vec!["pkg".to_owned()]);
        let mut modules: Vec<ParsedModule> = files
            .iter()
            .map(|(path, source)| {
                let module = ruff_python_parser::parse_module(source).expect("parse");
                let lines = LineIndex::new(source);
                let mut visitor = ModuleVisitor::new(path, &layout, FileContext::Runtime, &lines);
                visitor.visit_module(source, module.suite());
                visitor.into_parsed()
            })
            .collect();
        apply_import_checks(&mut modules, &layout);
        modules
    }

    fn optional(modules: &[ParsedModule], path: &str) -> Vec<(String, bool)> {
        modules
            .iter()
            .find(|module| module.path == path)
            .expect("module")
            .imports
            .iter()
            .filter(|import| !import.module.starts_with("pkg"))
            .map(|import| (import.module.clone(), import.optional))
            .collect()
    }

    const HELPERS: &str = "\
def check_x_support(caller):
    try:
        import x  # noqa
    except ImportError as e:
        raise ImportError(f\"{caller} requires x\") from e


def load_y():
    try:
        import y
        return y
    except (ModuleNotFoundError, OSError):
        raise


def probe_z():
    try:
        import z
    except ImportError:
        return False
    return True


def load_settings():
    try:
        import w
    except ModuleNotFoundError:
        log(\"no w\")
    except ImportError:
        raise
";

    /// sklearn's `check_matplotlib_support()` before `import matplotlib.pyplot`.
    #[test]
    fn imports_after_a_first_party_install_check_are_optional() {
        let modules = parse(&[
            (
                "src/pkg/__init__.py",
                "from pkg._helpers import check_x_support\n",
            ),
            ("src/pkg/_helpers.py", HELPERS),
            (
                "src/pkg/feature.py",
                "\
from pkg._helpers import check_x_support, load_settings, load_y, probe_z
from pkg import _helpers as helpers
import pkg


def plot():
    import x.early
    check_x_support(\"plot\")
    import x.pyplot as plt
    if plt:
        from x import colors


def table():
    y = load_y()
    import y
    import y.extra


def draw():
    helpers.check_x_support(\"draw\")
    import x.a


def reexported():
    pkg.check_x_support(\"draw\")
    import x.b


def other():
    probe_z()
    import z
    load_settings()
    import w
    import x.c
    if flag:
        check_x_support(\"other\")
    import x.d
",
            ),
        ]);
        assert_eq!(
            optional(&modules, "src/pkg/feature.py"),
            [
                ("x.early".to_owned(), false),
                ("x.pyplot".to_owned(), true),
                ("x".to_owned(), true),
                ("y".to_owned(), true),
                ("y.extra".to_owned(), true),
                ("x.a".to_owned(), true),
                ("x.b".to_owned(), true),
                // `probe_z` and `load_settings` return either way; the
                // other check ran in a branch that is over.
                ("z".to_owned(), false),
                ("w".to_owned(), false),
                ("x.c".to_owned(), false),
                ("x.d".to_owned(), false),
            ]
        );
    }

    /// A check defined in the same module, called at module level, covers
    /// the module's later imports and the functions defined after it.
    #[test]
    fn a_module_level_check_covers_later_imports() {
        let modules = parse(&[(
            "src/pkg/plots.py",
            &format!(
                "{HELPERS}\ncheck_x_support(\"plots\")\nimport x.pyplot\n\ndef f():\n    import x.colors\n"
            ),
        )]);
        let after: Vec<_> = optional(&modules, "src/pkg/plots.py")
            .into_iter()
            .filter(|(module, _)| module.starts_with("x."))
            .collect();
        assert_eq!(
            after,
            [("x.pyplot".to_owned(), true), ("x.colors".to_owned(), true)]
        );
    }

    #[test]
    fn checks_cover_submodules_and_parent_packages_only() {
        let checked = ["x.sub".to_owned()];
        assert!(checks_import(&checked, "x.sub.deep"));
        assert!(checks_import(&checked, "x"));
        assert!(!checks_import(&checked, "x.other"));
        assert!(!checks_import(&checked, "xy"));
        assert!(!checks_import(&checked, ""));
    }
}
