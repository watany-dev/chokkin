//! `__init__.py` re-export detection.

use std::collections::HashMap;

use crate::parser::{ImportKind, ParsedModule, file_module_name, resolve_relative_import};
use crate::sources::LayoutInfo;

use super::graph::SymbolId;

/// A name re-exported from a package `__init__.py`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReExport {
    /// Package module name (`acme` for `src/acme/__init__.py`).
    pub package_module: String,
    /// Re-exported name in the package namespace.
    pub name: String,
    /// Resolved source module for the imported symbol.
    pub source_module: String,
    /// Root-relative file path.
    pub path: String,
    /// 1-based source line.
    pub line: u32,
}

/// Collect relative re-exports from package `__init__.py` files.
pub(super) fn collect_reexports(
    modules: &[&ParsedModule],
    module_names: &HashMap<&str, String>,
    layout: &LayoutInfo,
) -> Vec<ReExport> {
    let mut reexports = Vec::new();

    for module in modules {
        if !module.path.ends_with("__init__.py") {
            continue;
        }
        let Some(package_module) = module_names
            .get(module.path.as_str())
            .cloned()
            .or_else(|| file_module_name(&module.path, layout))
        else {
            continue;
        };

        for import in &module.imports {
            if import.kind != ImportKind::ImportFrom || import.relative_level == 0 {
                continue;
            }
            let Some(name) = import.name.as_ref() else {
                continue;
            };
            if name.starts_with('_') && !module.exports.iter().any(|export| export == name) {
                continue;
            }

            let imported_name = if import.module.is_empty() {
                Some(name.as_str())
            } else {
                None
            };
            let module_suffix = import.module.as_str();
            let module_suffix = if module_suffix.is_empty() {
                None
            } else {
                Some(module_suffix)
            };
            let Some(source_module) = resolve_relative_import(
                &module.path,
                layout,
                import.relative_level,
                module_suffix,
                imported_name,
            ) else {
                continue;
            };

            reexports.push(ReExport {
                package_module: package_module.clone(),
                name: name.clone(),
                source_module,
                path: module.path.clone(),
                line: import.line,
            });
        }
    }

    reexports
}

/// Returns `true` when the package-level re-export name is referenced.
pub(super) fn is_reexport_used(
    reexport: &ReExport,
    references: &super::graph::ReferenceIndex<'_>,
) -> bool {
    let package_symbol = SymbolId::new(&reexport.package_module, &reexport.name);
    references.is_referenced(&package_symbol)
}
