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

/// Returns `true` when `path` resolves under `root`.
#[must_use]
pub fn path_is_within_root(root: &Path, path: &Path) -> bool {
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canonical_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical_path.starts_with(&canonical_root)
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
    pub label: String,
    /// 1-based line number when available.
    pub line: Option<u32>,
}

/// Parse `raw` and append either a dependency or a non-fatal warning.
pub fn push_dependency(push: DependencyPush<'_>) {
    let origin = DependencyOrigin {
        file: push.file.to_owned(),
        line: push.line,
        label: push.label,
    };
    match parse_requirement(push.raw, push.context, origin) {
        Ok(dep) => push.dependencies.push(dep),
        Err(warning) => push.warnings.push(warning),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_strips_root_prefix() {
        let root = Path::new("/proj");
        assert_eq!(relative_path(root, Path::new("/proj/a/b.txt")), "a/b.txt");
    }

    #[test]
    fn relative_path_falls_back_to_full_path_outside_root() {
        let root = Path::new("/proj");
        assert_eq!(
            relative_path(root, Path::new("/other/x.txt")),
            "/other/x.txt"
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;
        use std::path::PathBuf;

        fn rel_segments() -> impl Strategy<Value = Vec<String>> {
            prop::collection::vec("[a-z][a-z0-9_.]{0,10}", 1..4)
        }

        proptest! {
            #[test]
            fn relative_path_roundtrips_paths_under_root(segments in rel_segments()) {
                let root = PathBuf::from("/proj");
                let mut path = root.clone();
                for segment in &segments {
                    path.push(segment);
                }

                prop_assert_eq!(relative_path(&root, &path), segments.join("/"));
            }

            #[test]
            fn relative_path_never_yields_backslashes(
                root in "[a-zA-Z0-9/_.-]{0,30}",
                path in "\\PC{0,60}",
            ) {
                let result = relative_path(Path::new(&root), Path::new(&path));
                prop_assert!(!result.contains('\\'));
            }

            #[test]
            fn push_dependency_appends_exactly_one_item(raw in "\\PC{0,120}") {
                let mut dependencies = Vec::new();
                let mut warnings = Vec::new();
                push_dependency(DependencyPush {
                    dependencies: &mut dependencies,
                    warnings: &mut warnings,
                    raw: &raw,
                    context: DependencyContext::Runtime,
                    file: "requirements.txt",
                    label: "requirements.txt".to_string(),
                    line: Some(1),
                });

                prop_assert_eq!(dependencies.len() + warnings.len(), 1);
            }
        }
    }
}
