//! Shared helpers for manifest extraction modules.

use std::path::Path;

use super::error::ManifestError;
use super::pep508_util::parse_requirement;
use super::types::{DeclaredDependency, DependencyContext, DependencyOrigin};
use super::warnings::ManifestWarning;

/// Root-relative path for manifest origin reporting.
#[must_use]
pub fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| path.to_string_lossy().replace('\\', "/"),
        |p| p.to_string_lossy().replace('\\', "/"),
    )
}

/// Read a manifest file as UTF-8 text.
pub fn read_to_string(path: &Path) -> Result<String, ManifestError> {
    std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Context for pushing a parsed dependency declaration.
pub struct DependencyPush<'a> {
    /// Target dependency list.
    pub dependencies: &'a mut Vec<DeclaredDependency>,
    /// Warning sink.
    pub warnings: &'a mut Vec<ManifestWarning>,
    /// Raw PEP 508 or requirements line.
    pub raw: &'a str,
    /// Declaration context.
    pub context: DependencyContext,
    /// Root-relative manifest file path.
    pub file: &'a str,
    /// TOML key path or requirements label.
    pub label: &'a str,
    /// 1-based line number when available.
    pub line: Option<u32>,
}

/// Parse `raw` and append either a dependency or a non-fatal warning.
pub fn push_dependency(push: DependencyPush<'_>) {
    let origin = DependencyOrigin {
        file: push.file.to_owned(),
        line: push.line,
        label: push.label.to_owned(),
    };
    match parse_requirement(push.raw, push.context, origin) {
        Ok(dep) => push.dependencies.push(dep),
        Err(warning) => push.warnings.push(warning),
    }
}
