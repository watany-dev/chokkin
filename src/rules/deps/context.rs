//! Dependency context matching helpers (§10).

use crate::config::{ChokkinConfig, DependencyGroupsConfig};
use crate::manifest::DependencyContext;
use crate::parser::ImportContext;
use crate::sources::{DiscoveredSources, FileContext, assign_file_context};

/// Which side of a dependency declaration or usage we classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum UsageContext {
    /// Runtime application code.
    Runtime,
    /// Type-checking only.
    Type,
    /// Test files and test-only imports.
    Test,
    /// Documentation tree.
    Docs,
    /// Developer tooling files.
    Dev,
}

/// Broad declaration bucket for duplicate detection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum DeclarationBucket {
    /// `[project.dependencies]` and runtime groups.
    Runtime,
    /// Dev / test dependency groups.
    Dev,
    /// Type-checking groups.
    Type,
    /// Optional extra name.
    Optional(String),
}

impl DeclarationBucket {
    /// Stable label for CHK009 messages.
    pub(super) fn label(&self) -> String {
        match self {
            Self::Runtime => "runtime".to_owned(),
            Self::Dev => "dev".to_owned(),
            Self::Type => "type".to_owned(),
            Self::Optional(extra) => format!("optional:{extra}"),
        }
    }
}

/// Classify a declared dependency for context matching.
#[must_use]
pub(super) fn declaration_bucket(
    context: &DependencyContext,
    groups: &DependencyGroupsConfig,
) -> DeclarationBucket {
    match context {
        DependencyContext::Runtime => DeclarationBucket::Runtime,
        DependencyContext::Group(name) => {
            if groups.type_groups.iter().any(|group| group == name) {
                DeclarationBucket::Type
            } else if groups.dev_groups.iter().any(|group| group == name) {
                DeclarationBucket::Dev
            } else if groups.runtime_groups.iter().any(|group| group == name) {
                DeclarationBucket::Runtime
            } else {
                DeclarationBucket::Dev
            }
        },
        DependencyContext::OptionalExtra(extra) | DependencyContext::SetupExtra(extra) => {
            DeclarationBucket::Optional(extra.clone())
        },
    }
}

/// Whether a declaration satisfies usage in the given context.
#[must_use]
pub(super) fn declaration_matches_usage(
    context: &DependencyContext,
    usage: UsageContext,
    config: &ChokkinConfig,
) -> bool {
    let bucket = declaration_bucket(context, &config.dependencies);
    match usage {
        UsageContext::Runtime => matches!(
            bucket,
            DeclarationBucket::Runtime | DeclarationBucket::Optional(_)
        ),
        UsageContext::Type => matches!(
            bucket,
            DeclarationBucket::Type | DeclarationBucket::Runtime | DeclarationBucket::Optional(_)
        ),
        UsageContext::Test | UsageContext::Docs | UsageContext::Dev => matches!(
            bucket,
            DeclarationBucket::Dev | DeclarationBucket::Runtime | DeclarationBucket::Optional(_)
        ),
    }
}

/// Derive usage context from import metadata and file path.
#[must_use]
pub(super) fn usage_context_for_import(
    file: &str,
    import_context: ImportContext,
    sources: &DiscoveredSources,
) -> UsageContext {
    match import_context {
        ImportContext::Type => UsageContext::Type,
        ImportContext::Test => UsageContext::Test,
        ImportContext::Runtime => match assign_file_context(file, &sources.layout) {
            FileContext::Test => UsageContext::Test,
            FileContext::Docs => UsageContext::Docs,
            FileContext::Dev => UsageContext::Dev,
            FileContext::Runtime => UsageContext::Runtime,
        },
    }
}

/// Whether a declaration is considered directly declared for the usage context.
#[must_use]
pub(super) fn is_directly_declared(
    declarations: &[&crate::manifest::DeclaredDependency],
    usage: UsageContext,
    config: &ChokkinConfig,
) -> bool {
    declarations
        .iter()
        .any(|dep| declaration_matches_usage(&dep.context, usage, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::manifest::{DeclaredDependency, DependencyOrigin};
    use crate::sources::{DiscoveredSources, LayoutInfo, ProjectLayout};

    fn dep(context: DependencyContext) -> DeclaredDependency {
        DeclaredDependency {
            name: "pytest".to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(1),
                label: "test".to_owned(),
            },
            opaque: false,
        }
    }

    fn empty_sources() -> DiscoveredSources {
        DiscoveredSources {
            root: crate::discovery::ProjectRoot {
                path: std::env::temp_dir(),
                marker: crate::discovery::RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn runtime_usage_accepts_runtime_declaration() {
        let config = default_config();
        let runtime_dep = dep(DependencyContext::Runtime);
        let declarations = vec![&runtime_dep];
        assert!(is_directly_declared(
            &declarations,
            UsageContext::Runtime,
            &config
        ));
    }

    #[test]
    fn runtime_usage_rejects_dev_only_declaration() {
        let config = default_config();
        let dev_dep = dep(DependencyContext::Group("dev".to_owned()));
        let declarations = vec![&dev_dep];
        assert!(!is_directly_declared(
            &declarations,
            UsageContext::Runtime,
            &config
        ));
    }

    #[test]
    fn test_usage_accepts_runtime_declaration() {
        let config = default_config();
        let runtime_dep = dep(DependencyContext::Runtime);
        let declarations = vec![&runtime_dep];
        assert!(is_directly_declared(
            &declarations,
            UsageContext::Test,
            &config
        ));
    }

    #[test]
    fn classifies_src_runtime_file() {
        let sources = empty_sources();
        assert_eq!(
            usage_context_for_import("src/acme/app.py", ImportContext::Runtime, &sources),
            UsageContext::Runtime
        );
    }
}
