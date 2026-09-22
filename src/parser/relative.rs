//! Relative import normalization using project layout.

use crate::sources::{LayoutInfo, path_to_module};

use super::types::ParseDiagnostic;
use super::types::ParseSeverity;

/// Resolve a file path to its dotted module name.
///
/// Kept as the historical name for [`crate::sources::path_to_module`], which is
/// now the single source of truth: this function used to derive module names of
/// its own and could disagree with the `ModuleIndex` key for the same file.
#[must_use]
pub fn file_module_name(path: &str, layout: &LayoutInfo) -> Option<String> {
    path_to_module(path, layout)
}

/// Resolve a relative import to an absolute dotted module name.
///
/// Returns `None` when the import cannot be resolved (caller records a diagnostic).
#[must_use]
pub fn resolve_relative_import(
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

    imported_name.map(|name| join_module(&base, name))
}

/// Build a diagnostic for an unresolved relative import.
#[must_use]
pub fn unresolved_relative_diagnostic(path: &str, line: u32) -> ParseDiagnostic {
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

fn ascend_package(package: &str, level: u8) -> Option<String> {
    if level == 0 {
        return Some(package.to_owned());
    }
    let mut current = package.to_owned();
    for _ in 1..level {
        if current.is_empty() {
            return None;
        }
        current = parent_of(&current)?;
    }
    Some(current)
}

fn parent_of(package: &str) -> Option<String> {
    if package.is_empty() {
        None
    } else if let Some((parent, _)) = package.rsplit_once('.') {
        Some(parent.to_owned())
    } else {
        Some(String::new())
    }
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
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        }
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
    fn relative_import_from_file_outside_the_index_is_unresolved() {
        // `tests/` is walked under an Unknown layout but has no ModuleIndex
        // entry, so a name derived for it could never be looked up.
        let layout = LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        assert_eq!(
            resolve_relative_import("tests/unit/test_core.py", &layout, 1, None, Some("helpers")),
            None
        );
    }

    #[test]
    fn relative_import_under_unknown_layout_drops_src_prefix() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        assert_eq!(
            resolve_relative_import("src/acme/api/__init__.py", &layout, 1, Some("models"), None),
            Some("acme.api.models".to_owned())
        );
    }

    #[test]
    fn unresolved_without_package_context() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Unknown,
            packages: Vec::new(),
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        assert!(resolve_relative_import("routes.py", &layout, 1, None, Some("sibling")).is_none());
    }
}
