//! Relative import normalization using project layout.

use crate::sources::{LayoutInfo, path_to_module};

use super::types::ParseDiagnostic;
use super::types::ParseSeverity;

/// Resolve a relative import to an absolute dotted module name.
///
/// Returns `None` when the import cannot be resolved (caller records a diagnostic).
#[must_use]
pub(crate) fn resolve_relative_import(
    file_path: &str,
    layout: &LayoutInfo,
    level: u8,
    module_suffix: Option<&str>,
    imported_name: Option<&str>,
) -> Option<String> {
    if level == 0 {
        return module_suffix.map(str::to_owned);
    }

    let current_module = path_to_module(file_path, layout)?;
    let is_init = file_path.ends_with("__init__.py");
    let containing_package = containing_package(&current_module, is_init);
    if containing_package.is_empty() && level > 0 {
        return None;
    }

    let base = ascend_package(&containing_package, level)?;

    if let Some(suffix) = module_suffix.filter(|value| !value.is_empty()) {
        return Some(join_module(&base, suffix));
    }

    // `from . import *` loads the package itself, not a `*` submodule.
    imported_name.map(|name| {
        if name == "*" {
            base
        } else {
            join_module(&base, name)
        }
    })
}

/// `__package__` of the module at `file_path`: the module itself for a
/// package `__init__.py`, its parent otherwise.
#[must_use]
pub(super) fn module_package(file_path: &str, layout: &LayoutInfo) -> Option<String> {
    let current_module = path_to_module(file_path, layout)?;
    Some(containing_package(
        &current_module,
        file_path.ends_with("__init__.py"),
    ))
}

/// Absolute name of a dotted relative `name` against `package`, as
/// `importlib.util.resolve_name` computes it.
#[must_use]
pub(super) fn resolve_relative_name(name: &str, package: &str) -> Option<String> {
    let suffix = name.trim_start_matches('.');
    let level = u8::try_from(name.len() - suffix.len()).ok()?;
    let base = ascend_package(package, level)?;
    Some(if suffix.is_empty() {
        base
    } else {
        join_module(&base, suffix)
    })
}

/// Build a diagnostic for an unresolved relative import.
#[must_use]
pub(super) fn unresolved_relative_diagnostic(path: &str, line: u32) -> ParseDiagnostic {
    ParseDiagnostic {
        line,
        message: format!("could not resolve relative import in `{path}` (missing package context)"),
        severity: ParseSeverity::Warning,
    }
}

fn containing_package(module: &str, is_init: bool) -> String {
    if is_init {
        module.to_owned()
    } else if let Some((package, _)) = module.rsplit_once('.') {
        package.to_owned()
    } else {
        String::new()
    }
}

/// Ascend `level - 1` packages, mirroring `CPython`'s
/// `importlib._bootstrap._resolve_name`: `package.rsplit('.', level - 1)` must
/// yield at least `level` parts, otherwise the import reaches beyond the
/// top-level package and is an `ImportError`.
fn ascend_package(package: &str, level: u8) -> Option<String> {
    if level == 0 {
        return Some(package.to_owned());
    }
    let mut current = package.to_owned();
    for _ in 1..level {
        current = parent_of(&current)?;
    }
    if current.is_empty() {
        None
    } else {
        Some(current)
    }
}

fn parent_of(package: &str) -> Option<String> {
    package
        .rsplit_once('.')
        .map(|(parent, _)| parent.to_owned())
}

fn join_module(base: &str, suffix: &str) -> String {
    if base.is_empty() {
        suffix.to_owned()
    } else {
        format!("{base}.{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::ProjectLayout;

    fn src_layout() -> LayoutInfo {
        LayoutInfo {
            layout: ProjectLayout::Src,
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            ..Default::default()
        }
    }

    #[test]
    fn resolve_relative_name_mirrors_importlib() {
        assert_eq!(
            resolve_relative_name(".sub", "acme"),
            Some("acme.sub".to_owned())
        );
        assert_eq!(
            resolve_relative_name("..", "acme.api"),
            Some("acme".to_owned())
        );
        assert_eq!(resolve_relative_name("..sub", "acme"), None);
        assert_eq!(resolve_relative_name(".sub", ""), None);
    }

    #[test]
    fn resolve_parent_relative_import() {
        let layout = src_layout();
        let resolved =
            resolve_relative_import("src/acme/api/routes.py", &layout, 2, Some("models"), None);
        assert_eq!(resolved, Some("acme.models".to_owned()));
    }

    #[test]
    fn resolve_sibling_relative_import() {
        let layout = src_layout();
        let resolved =
            resolve_relative_import("src/acme/api/routes.py", &layout, 1, None, Some("sibling"));
        assert_eq!(resolved, Some("acme.api.sibling".to_owned()));
    }

    #[test]
    fn relative_star_import_resolves_to_the_package() {
        let layout = src_layout();
        assert_eq!(
            resolve_relative_import("src/acme/api/__init__.py", &layout, 1, None, Some("*")),
            Some("acme.api".to_owned())
        );
    }

    #[test]
    fn relative_import_from_file_outside_the_index_is_unresolved() {
        // `tests/` is walked under an Unknown layout but has no ModuleIndex
        // entry, so a name derived for it could never be looked up.
        let layout = LayoutInfo {
            packages: vec!["acme".to_owned()],
            ..Default::default()
        };
        assert_eq!(
            resolve_relative_import("tests/unit/test_core.py", &layout, 1, None, Some("helpers")),
            None
        );
    }

    #[test]
    fn relative_import_beyond_top_level_package_is_unresolved() {
        let layout = src_layout();
        // `from .. import thing` inside the top-level package: CPython raises
        // `ImportError: attempted relative import beyond top-level package`.
        assert_eq!(
            resolve_relative_import("src/acme/__init__.py", &layout, 2, None, Some("thing")),
            None
        );
        assert_eq!(
            resolve_relative_import("src/acme/core.py", &layout, 2, Some("models"), None),
            None
        );
        assert_eq!(
            resolve_relative_import("src/acme/api/routes.py", &layout, 3, Some("models"), None),
            None
        );
    }

    #[test]
    fn relative_import_under_unknown_layout_drops_src_prefix() {
        let layout = LayoutInfo::default();
        assert_eq!(
            resolve_relative_import("src/acme/api/__init__.py", &layout, 1, Some("models"), None),
            Some("acme.api.models".to_owned())
        );
    }

    #[test]
    fn unresolved_without_package_context() {
        // `src/routes.py` maps to the top-level module `routes`, which has no
        // containing package: CPython raises "attempted relative import with
        // no known parent package".
        let layout = src_layout();
        assert_eq!(
            resolve_relative_import("src/routes.py", &layout, 1, None, Some("sibling")),
            None
        );
        assert_eq!(
            resolve_relative_import("src/routes.py", &layout, 1, Some("models"), None),
            None
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// `importlib._bootstrap._resolve_name` plus PEP 366 `__package__`
        /// derivation, written independently of the implementation:
        /// `package.rsplit('.', level - 1)` must yield `level` parts.
        fn cpython_resolve(
            module: &[String],
            is_init: bool,
            level: u8,
            suffix: Option<&str>,
            name: Option<&str>,
        ) -> Option<String> {
            if level == 0 {
                return suffix.map(str::to_owned);
            }
            let package_parts = if is_init {
                module
            } else {
                &module[..module.len() - 1]
            };
            let level = usize::from(level);
            if package_parts.len() < level {
                return None;
            }
            let base = package_parts[..=package_parts.len() - level].join(".");
            match (suffix.filter(|value| !value.is_empty()), name) {
                (Some(suffix), _) => Some(format!("{base}.{suffix}")),
                (None, Some("*")) => Some(base),
                (None, Some(name)) => Some(format!("{base}.{name}")),
                (None, None) => None,
            }
        }

        fn segment() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9_]{0,3}"
        }

        fn module() -> impl Strategy<Value = Vec<String>> {
            prop::collection::vec(segment(), 1..5)
        }

        fn suffix() -> impl Strategy<Value = Option<String>> {
            prop_oneof![
                Just(None),
                Just(Some(String::new())),
                prop::collection::vec(segment(), 1..3).prop_map(|parts| Some(parts.join("."))),
            ]
        }

        fn imported_name() -> impl Strategy<Value = Option<String>> {
            prop_oneof![
                Just(None),
                Just(Some("*".to_owned())),
                segment().prop_map(Some)
            ]
        }

        fn file_path(prefix: &str, module: &[String], is_init: bool) -> String {
            let stem = format!("{prefix}{}", module.join("/"));
            if is_init {
                format!("{stem}/__init__.py")
            } else {
                format!("{stem}.py")
            }
        }

        fn flat_layout(packages: Vec<String>) -> LayoutInfo {
            LayoutInfo {
                layout: ProjectLayout::Flat,
                packages,
                ..Default::default()
            }
        }

        proptest! {
            #[test]
            fn resolution_matches_cpython(
                module in module(),
                is_init in any::<bool>(),
                level in 0u8..6,
                suffix in suffix(),
                name in imported_name(),
                flat in any::<bool>(),
            ) {
                let (layout, path) = if flat {
                    (flat_layout(vec![module[0].clone()]), file_path("", &module, is_init))
                } else {
                    (src_layout(), file_path("src/", &module, is_init))
                };
                prop_assert_eq!(
                    resolve_relative_import(&path, &layout, level, suffix.as_deref(), name.as_deref()),
                    cpython_resolve(&module, is_init, level, suffix.as_deref(), name.as_deref())
                );
            }

            #[test]
            fn file_outside_every_package_is_unresolved(
                module in module(),
                is_init in any::<bool>(),
                level in 1u8..6,
                suffix in suffix(),
                name in imported_name(),
            ) {
                // `tests/` is neither `src/` nor a listed package, so the file
                // has no module name the index could hold.
                let layout = flat_layout(vec!["acme".to_owned()]);
                let path = file_path("tests/", &module, is_init);
                prop_assert_eq!(
                    resolve_relative_import(&path, &layout, level, suffix.as_deref(), name.as_deref()),
                    None
                );
            }
        }
    }
}
