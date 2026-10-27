//! Imports made after a call to a first-party install check (#1269).
//!
//! A project may check an optional package in a helper of its own before
//! importing it: the helper tries the import and raises from its `except
//! ImportError:` handler, so the code after the call runs only with the
//! package. The parser records the helpers each module defines
//! ([`ParsedModule::import_checks`]) and the calls each import runs after
//! ([`ImportRef::called_before`]); only the whole project tells which call
//! reaches which helper.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use crate::sources::{LayoutInfo, module_path, path_to_module};

use super::types::{ImportCheck, ImportKind, ImportRef, ParsedModule};

/// Re-exports followed from a callee to the helper's definition; a chain
/// longer than this is a cycle or too far to trust.
const MAX_HOPS: usize = 8;

/// Mark each import optional that runs after a call reaching a first-party
/// function which checks a package is installed: a statement call to one
/// that raises without it, or a call a branch test needs truthy to one that
/// raises or returns a falsy value (#1272).
///
/// A callee is read through the calling module's imports (`from .utils
/// import check`, `import pkg.utils as u; u.check(...)`) or as a function of
/// the module itself, then through the re-exports of the modules it names,
/// such as a package `__init__.py`. The code after the call runs only with
/// the checked package, so every import there is optional, whatever it
/// imports, as in a `try`'s `else:` (#1272). A callee that reaches nothing
/// leaves the import as it is.
pub(crate) fn apply_import_checks(modules: &mut [ParsedModule], layout: &LayoutInfo) {
    let functions = check_names(modules);
    // Most calls before an import (`logger.info(...)`) can name no check;
    // the name alone rules them out without resolving anything.
    let may_check =
        |callee: &String| functions.contains(callee.rsplit('.').next().unwrap_or(callee));
    let callers: Vec<usize> = modules
        .iter()
        .enumerate()
        .filter(|(_, module)| {
            module.imports.iter().any(|import| {
                import.called_before.iter().any(may_check)
                    || import.tested_before.iter().any(may_check)
            })
        })
        .map(|(position, _)| position)
        .collect();
    if callers.is_empty() {
        return;
    }
    let index = ProjectIndex::new(modules, layout);
    let marks: Vec<(usize, Vec<usize>)> = callers
        .into_iter()
        .map(|caller| {
            let module = &modules[caller];
            let marked =
                module
                    .imports
                    .iter()
                    .enumerate()
                    .filter(|(_, import)| {
                        let check = |callee: &String| {
                            may_check(callee)
                                .then(|| index.check(caller, callee))
                                .flatten()
                        };
                        !import.module.is_empty()
                            && (import.called_before.iter().any(|callee| {
                                check(callee).is_some_and(|check| !check.returns_falsy)
                            }) || import
                                .tested_before
                                .iter()
                                .any(|callee| check(callee).is_some()))
                    })
                    .map(|(position, _)| position)
                    .collect();
            (caller, marked)
        })
        .collect();
    for (caller, marked) in marks {
        for position in marked {
            modules[caller].imports[position].optional = true;
        }
    }
}

/// The names a check can be called by: each check function's own name, and
/// the `as` names re-exports give it (`from ._deps import needs as require`).
fn check_names(modules: &[ParsedModule]) -> HashSet<&str> {
    let mut names: HashSet<&str> = modules
        .iter()
        .flat_map(|module| &module.import_checks)
        .map(|check| check.function.as_str())
        .collect();
    let renames: Vec<(&str, &str)> = modules
        .iter()
        .flat_map(|module| &module.imports)
        .filter_map(|import| Some((import.name.as_deref()?, import.alias.as_deref()?)))
        .collect();
    for _ in 0..MAX_HOPS {
        let before = names.len();
        for (name, alias) in &renames {
            if names.contains(name) {
                names.insert(alias);
            }
        }
        if names.len() == before {
            break;
        }
    }
    names
}

/// The project's install checks, and its modules by name for following
/// re-exports. A module's name is worked out only when a lookup needs it:
/// with many workspace members that is the costly part.
struct ProjectIndex<'a> {
    modules: &'a [ParsedModule],
    layout: &'a LayoutInfo,
    names: Vec<OnceCell<Option<String>>>,
    /// The last part of each module's path (`utils` for `pkg/utils.py` and
    /// `pkg/utils/__init__.py`) to the modules that may carry a name ending
    /// in it.
    by_last_part: HashMap<&'a str, Vec<usize>>,
    /// `pkg.mod.function` to the check it is.
    checks: HashMap<String, &'a ImportCheck>,
}

impl<'a> ProjectIndex<'a> {
    fn new(modules: &'a [ParsedModule], layout: &'a LayoutInfo) -> Self {
        let mut by_last_part: HashMap<&str, Vec<usize>> = HashMap::new();
        for (position, module) in modules.iter().enumerate() {
            if let Some(stem) = module_path(&module.path) {
                let last = stem.rsplit('/').next().unwrap_or(stem);
                by_last_part.entry(last).or_default().push(position);
            }
        }
        let mut index = Self {
            modules,
            layout,
            names: vec![OnceCell::new(); modules.len()],
            by_last_part,
            checks: HashMap::new(),
        };
        for (position, module) in modules.iter().enumerate() {
            if module.import_checks.is_empty() {
                continue;
            }
            let Some(name) = index.name(position).map(str::to_owned) else {
                continue;
            };
            for check in &module.import_checks {
                index
                    .checks
                    .insert(format!("{name}.{}", check.function), check);
            }
        }
        index
    }

    fn name(&self, position: usize) -> Option<&str> {
        self.names[position]
            .get_or_init(|| path_to_module(&self.modules[position].path, self.layout))
            .as_deref()
    }

    fn module_named(&self, name: &str) -> Option<&'a ParsedModule> {
        let last = name.rsplit('.').next().unwrap_or(name);
        let position = *self
            .by_last_part
            .get(last)?
            .iter()
            .find(|&&position| self.name(position) == Some(name))?;
        Some(&self.modules[position])
    }

    /// The install check the function `callee` names in the module at
    /// `calling` is, if it is one.
    fn check(&self, calling: usize, callee: &str) -> Option<&'a ImportCheck> {
        let (head, rest) = match callee.split_once('.') {
            Some((head, rest)) => (head, Some(rest)),
            None => (callee, None),
        };
        let base = match binding_target(&self.modules[calling].imports, head) {
            Some(target) => target,
            None => format!("{}.{head}", self.name(calling)?),
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
            let owner = self.module_named(owner)?;
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
    import other


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
                // Whatever it imports, the code after the check runs only
                // with `x` (#1272).
                ("other".to_owned(), true),
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

    /// A check reached under names re-exports give it, one rename after
    /// another (`a.py` renames what `b.py` renamed), still counts; a name
    /// no check carries, or a check's name bound to nothing, reaches nothing.
    #[test]
    fn renamed_re_exports_reach_the_check() {
        let modules = parse(&[
            ("src/pkg/a.py", "from pkg.b import require as require_x\n"),
            (
                "src/pkg/b.py",
                "from pkg._helpers import check_x_support as require\n",
            ),
            ("src/pkg/_helpers.py", HELPERS),
            (
                "src/pkg/stray.py",
                "def h():\n    check_x_support(\"h\")\n    import x.stray\n",
            ),
            (
                "src/pkg/use.py",
                "from pkg.a import require_x\nfrom pkg.b import other\n\n\ndef f():\n    require_x(\"f\")\n    import x.renamed\n\n\ndef g():\n    other()\n    import x.other\n",
            ),
        ]);
        assert_eq!(
            optional(&modules, "src/pkg/use.py"),
            [
                ("x.renamed".to_owned(), true),
                ("x.other".to_owned(), false)
            ]
        );
        // A check's name that no import binds and the module does not
        // define reaches nothing.
        assert_eq!(
            optional(&modules, "src/pkg/stray.py"),
            [("x.stray".to_owned(), false)]
        );
    }

    /// #1272: a check that returns `False` on a missing package proves it
    /// only where its result was tested truthy; called as a statement, it
    /// returns either way. A raising check counts in a test too.
    #[test]
    fn falsy_checks_count_only_where_tested() {
        let modules = parse(&[
            ("src/pkg/_helpers.py", HELPERS),
            (
                "src/pkg/feature.py",
                "\
from pkg._helpers import check_x_support, probe_z


def shown():
    if probe_z():
        import z.display
        import rich


def early():
    if not probe_z():
        return
    import z.early


def statement():
    probe_z()
    import z.statement


def raising():
    if check_x_support(\"raising\") is None:
        import x.none
    if not check_x_support(\"raising\") or flag:
        return
    import x.after
",
            ),
        ]);
        assert_eq!(
            optional(&modules, "src/pkg/feature.py"),
            [
                ("z.display".to_owned(), true),
                ("rich".to_owned(), true),
                ("z.early".to_owned(), true),
                ("z.statement".to_owned(), false),
                ("x.none".to_owned(), false),
                ("x.after".to_owned(), true),
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
}
