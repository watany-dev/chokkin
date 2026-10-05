//! Dependency context matching helpers (§10).

use std::collections::BTreeSet;

use crate::config::{ChokkinConfig, DependencyGroupsConfig};
use crate::manifest::{DeclaredDependency, DependencyContext};
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

/// Broad declaration bucket for context matching (CHK005 / CHK010).
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
    /// Stable label for CHK005 messages.
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
        DependencyContext::Group(name) => group_bucket(name, groups),
        DependencyContext::OptionalExtra(extra) | DependencyContext::SetupExtra(extra) => {
            DeclarationBucket::Optional(extra.clone())
        },
        // Build requirements never enter the declared index; this arm only
        // keeps the match total.
        DependencyContext::Build => DeclarationBucket::Dev,
    }
}

fn group_bucket(name: &str, groups: &DependencyGroupsConfig) -> DeclarationBucket {
    if groups.type_groups.iter().any(|group| group == name) {
        DeclarationBucket::Type
    } else if groups.dev_groups.iter().any(|group| group == name) {
        DeclarationBucket::Dev
    } else if groups.runtime_groups.iter().any(|group| group == name) {
        DeclarationBucket::Runtime
    } else {
        DeclarationBucket::Dev
    }
}

/// Buckets a declaration counts toward: its own context plus every group that
/// pulls it in through PEP 735 `include-group`.
#[must_use]
pub(super) fn declaration_buckets(
    dep: &DeclaredDependency,
    groups: &DependencyGroupsConfig,
) -> BTreeSet<DeclarationBucket> {
    let mut buckets = BTreeSet::from([declaration_bucket(&dep.context, groups)]);
    buckets.extend(
        dep.included_via
            .iter()
            .filter_map(|chain| chain.first())
            .map(|includer| group_bucket(includer, groups)),
    );
    buckets
}

/// `--explain` lines naming the include chains behind a group declaration.
#[must_use]
pub(super) fn include_path_details(dep: &DeclaredDependency) -> Vec<String> {
    dep.included_via
        .iter()
        .map(|chain| format!("included via dependency-groups: {}", chain.join(" -> ")))
        .collect()
}

/// Whether a declaration satisfies usage in the given context.
#[must_use]
fn declaration_matches_usage(
    dep: &DeclaredDependency,
    usage: UsageContext,
    groups: &DependencyGroupsConfig,
) -> bool {
    bucket_matches_usage(&declaration_bucket(&dep.context, groups), usage)
        || dep
            .included_via
            .iter()
            .filter_map(|chain| chain.first())
            .any(|includer| bucket_matches_usage(&group_bucket(includer, groups), usage))
}

fn bucket_matches_usage(bucket: &DeclarationBucket, usage: UsageContext) -> bool {
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
        ImportContext::Runtime => match file_context(file, sources) {
            FileContext::Test => UsageContext::Test,
            FileContext::Docs => UsageContext::Docs,
            FileContext::Dev => UsageContext::Dev,
            FileContext::Runtime => UsageContext::Runtime,
        },
    }
}

/// The context discovery gave `file` (pytest `testpaths` make it test),
/// falling back to its path for a file outside the discovered set.
fn file_context(file: &str, sources: &DiscoveredSources) -> FileContext {
    sources
        .files
        .binary_search_by(|candidate| candidate.path.as_str().cmp(file))
        .ok()
        .and_then(|index| sources.files.get(index))
        .map_or_else(|| assign_file_context(file), |found| found.context)
}

/// Whether a declaration is considered directly declared for the usage context.
#[must_use]
pub(super) fn is_directly_declared(
    declarations: &[&DeclaredDependency],
    usage: UsageContext,
    config: &ChokkinConfig,
) -> bool {
    declarations
        .iter()
        .any(|dep| declaration_matches_usage(dep, usage, &config.dependencies))
}

/// kani proofs of the context table behind `docs/dev/formal/deps_rules_z3.py`
/// S1/S2/W1, run against the implementation instead of a hand port
/// (`make kani`).
#[cfg(kani)]
mod verification {
    use super::*;
    use crate::manifest::DependencyOrigin;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Runtime,
        Dev,
        Type,
        Optional,
    }

    fn any_usage() -> UsageContext {
        match kani::any::<u8>() % 5 {
            0 => UsageContext::Runtime,
            1 => UsageContext::Type,
            2 => UsageContext::Test,
            3 => UsageContext::Docs,
            _ => UsageContext::Dev,
        }
    }

    fn any_bucket() -> (DeclarationBucket, Kind) {
        match kani::any::<u8>() % 4 {
            0 => (DeclarationBucket::Runtime, Kind::Runtime),
            1 => (DeclarationBucket::Dev, Kind::Dev),
            2 => (DeclarationBucket::Type, Kind::Type),
            _ => (
                DeclarationBucket::Optional("extra".to_owned()),
                Kind::Optional,
            ),
        }
    }

    /// `matches_usage` of `deps_rules_z3.py` (spec §10).
    fn spec_matches(kind: Kind, usage: UsageContext) -> bool {
        match usage {
            UsageContext::Runtime => matches!(kind, Kind::Runtime | Kind::Optional),
            UsageContext::Type => matches!(kind, Kind::Type | Kind::Runtime | Kind::Optional),
            UsageContext::Test | UsageContext::Docs | UsageContext::Dev => {
                matches!(kind, Kind::Dev | Kind::Runtime | Kind::Optional)
            },
        }
    }

    const GROUPS: [(&str, Kind); 4] = [
        ("dev", Kind::Dev),
        ("typing", Kind::Type),
        ("server", Kind::Runtime),
        // Unlisted groups fall back to dev.
        ("other", Kind::Dev),
    ];

    fn any_group() -> (&'static str, Kind) {
        GROUPS[usize::from(kani::any::<u8>() % 4)]
    }

    fn any_context() -> (DependencyContext, Kind) {
        match kani::any::<u8>() % 4 {
            0 => (DependencyContext::Runtime, Kind::Runtime),
            1 => {
                let (name, kind) = any_group();
                (DependencyContext::Group(name.to_owned()), kind)
            },
            2 => (
                DependencyContext::OptionalExtra("extra".to_owned()),
                Kind::Optional,
            ),
            _ => (
                DependencyContext::SetupExtra("extra".to_owned()),
                Kind::Optional,
            ),
        }
    }

    /// The table itself; S2 (a dev/type-only declaration is not "declared"
    /// for runtime usage, so it reaches CHK005 rather than CHK003/CHK004)
    /// rests on its runtime row.
    #[kani::proof]
    fn bucket_matches_usage_follows_spec_table() {
        let (bucket, kind) = any_bucket();
        let usage = any_usage();
        assert_eq!(
            bucket_matches_usage(&bucket, usage),
            spec_matches(kind, usage)
        );
    }

    /// A declaration counts for a usage when its own group or the group that
    /// includes it (PEP 735) does, under the configured group classification.
    #[kani::proof]
    #[kani::unwind(8)]
    fn declaration_matches_usage_follows_own_and_including_group() {
        let groups = DependencyGroupsConfig {
            dev_groups: vec!["dev".to_owned()],
            runtime_groups: vec!["server".to_owned()],
            type_groups: vec!["typing".to_owned()],
        };
        let (context, own_kind) = any_context();
        let includer = kani::any::<bool>().then(any_group);
        let dep = DeclaredDependency {
            name: "pkg".to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context,
            origin: DependencyOrigin {
                file: String::new(),
                line: None,
                label: String::new(),
            },
            opaque: false,
            included_via: includer
                .map(|(name, _)| vec![vec![name.to_owned(), "inner".to_owned()]])
                .unwrap_or_default(),
        };
        let usage = any_usage();

        let expected = spec_matches(own_kind, usage)
            || includer.is_some_and(|(_, kind)| spec_matches(kind, usage));
        assert_eq!(declaration_matches_usage(&dep, usage, &groups), expected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::manifest::DependencyOrigin;
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
            included_via: Vec::new(),
        }
    }

    fn empty_sources() -> DiscoveredSources {
        DiscoveredSources {
            root: crate::discovery::ProjectRoot {
                path: std::env::temp_dir(),
                marker: crate::discovery::RootMarker::PyProjectToml,
            },
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                package_root: "src".to_owned(),
                packages: vec!["acme".to_owned()],
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
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
    fn test_usage_accepts_type_group_included_by_dev_group() {
        let config = default_config();
        let mut typing_dep = dep(DependencyContext::Group("typing".to_owned()));
        assert!(!is_directly_declared(
            &[&typing_dep],
            UsageContext::Test,
            &config
        ));
        typing_dep.included_via = vec![vec!["dev".to_owned(), "typing".to_owned()]];
        assert!(is_directly_declared(
            &[&typing_dep],
            UsageContext::Test,
            &config
        ));
    }

    #[test]
    fn runtime_usage_accepts_group_included_by_runtime_group() {
        let config = default_config();
        let mut shared = dep(DependencyContext::Group("shared".to_owned()));
        shared.included_via = vec![vec!["server".to_owned(), "shared".to_owned()]];
        assert!(is_directly_declared(
            &[&shared],
            UsageContext::Runtime,
            &config
        ));
        assert_eq!(
            declaration_buckets(&shared, &config.dependencies),
            BTreeSet::from([DeclarationBucket::Dev, DeclarationBucket::Runtime])
        );
    }

    #[test]
    fn classifies_src_runtime_file() {
        let sources = empty_sources();
        assert_eq!(
            usage_context_for_import("src/acme/app.py", ImportContext::Runtime, &sources),
            UsageContext::Runtime
        );
    }

    #[test]
    fn discovered_context_wins_over_path() {
        let mut sources = empty_sources();
        sources.files.push(crate::sources::DiscoveredFile {
            path: "t/unit/helpers.py".to_owned(),
            kind: crate::sources::FileKind::Python,
            context: FileContext::Test,
        });
        assert_eq!(
            usage_context_for_import("t/unit/helpers.py", ImportContext::Runtime, &sources),
            UsageContext::Test
        );
    }
}
