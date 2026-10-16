//! Dependency context matching helpers (§10).

use std::collections::BTreeSet;

use crate::config::{ChokkinConfig, DependencyGroupsConfig};
use crate::manifest::{DeclaredDependency, DependencyContext};
use crate::parser::ImportContext;
use crate::sources::{DiscoveredSources, FileContext, assign_layout_file_context};

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
pub(crate) enum DeclarationBucket {
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
pub(crate) fn declaration_bucket(
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
pub(crate) fn declaration_buckets(
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
        ImportContext::Runtime | ImportContext::Dev => match file_context(file, sources) {
            FileContext::Test => UsageContext::Test,
            FileContext::Docs => UsageContext::Docs,
            FileContext::Dev => UsageContext::Dev,
            // A `__main__` block only demotes a runtime file (#681).
            FileContext::Runtime if import_context == ImportContext::Dev => UsageContext::Dev,
            FileContext::Runtime => UsageContext::Runtime,
        },
    }
}

/// The context discovery gave `file` (pytest `testpaths` make it test),
/// falling back to its path for a file outside the discovered set.
pub(crate) fn file_context(file: &str, sources: &DiscoveredSources) -> FileContext {
    sources
        .files
        .binary_search_by(|candidate| candidate.path.as_str().cmp(file))
        .ok()
        .and_then(|index| sources.files.get(index))
        .map_or_else(
            || assign_layout_file_context(file, &sources.layout),
            |found| found.context,
        )
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
    use crate::sources::{DiscoveredSources, LayoutInfo};

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
            layout: LayoutInfo::new("src", vec!["acme".to_owned()]),
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

    /// #681: a `__main__` block is dev only in a runtime file.
    #[test]
    fn main_block_is_dev_only_in_runtime_files() {
        let sources = empty_sources();
        for (file, expected) in [
            ("src/acme/app.py", UsageContext::Dev),
            ("docs/examples/plot.py", UsageContext::Docs),
            ("tests/test_app.py", UsageContext::Test),
        ] {
            assert_eq!(
                usage_context_for_import(file, ImportContext::Dev, &sources),
                expected,
                "{file}"
            );
        }
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

    /// Workspace members: `pkgs/m`, a member nested in it, and a member
    /// under root `examples/` (pydantic-ai).
    const MEMBERS: &[&str] = &["pkgs/m", "pkgs/m/plugins/p", "examples/demo"];

    /// Where a path starts: the root, a member, or `pkgs/mx`, which only
    /// shares a prefix with `pkgs/m`.
    const PREFIXES: &[&str] = &["", "pkgs/m", "pkgs/m/plugins/p", "examples/demo", "pkgs/mx"];

    const DIRS: &[&str] = &[
        "tests", "test", "testing", "docs", "scripts", "examples", "src",
    ];

    const FILES: &[&str] = &[
        "mod.py",
        "__main__.py",
        "conftest.py",
        "noxfile.py",
        "test_x.py",
        "x_test.pyi",
        "test_x.PY",
        "Test_x.py",
    ];

    const IMPORT_CONTEXTS: &[ImportContext] = &[
        ImportContext::Runtime,
        ImportContext::Type,
        ImportContext::Test,
        ImportContext::Dev,
    ];

    /// The §10 file-context table. `prefix` is a member or the root (`""`
    /// or a non-member directory), `dirs` the directories below it.
    fn file_context_model(prefix: &str, dirs: &[&str], file: &str) -> FileContext {
        let prefix_dirs: Vec<&str> = prefix.split('/').filter(|dir| !dir.is_empty()).collect();
        let test_name = file == "conftest.py"
            || file.rsplit_once('.').is_some_and(|(stem, ext)| {
                (ext.eq_ignore_ascii_case("py") || ext.eq_ignore_ascii_case("pyi"))
                    && (stem.starts_with("test_") || stem.ends_with("_test"))
            });
        if test_name || prefix_dirs.iter().chain(dirs).any(|dir| *dir == "tests") {
            return FileContext::Test;
        }
        // In a member, its own `test/`, `docs/` and `examples/` stand in for
        // the root's; root `scripts/` and `noxfile.py` have no member
        // counterpart.
        let member = MEMBERS.contains(&prefix);
        let top = if member {
            dirs.first().copied()
        } else {
            prefix_dirs.iter().chain(dirs).next().copied()
        };
        match top {
            Some("test") => FileContext::Test,
            Some("docs") => FileContext::Docs,
            Some("examples") => FileContext::Dev,
            Some("scripts") if !member => FileContext::Dev,
            None if !member && file == "noxfile.py" => FileContext::Dev,
            _ => FileContext::Runtime,
        }
    }

    /// §10: a `TYPE_CHECKING` or test import keeps its own context; a
    /// runtime import takes the file's; a `__main__` block import is dev
    /// only in a runtime file (#681).
    fn usage_model(import_context: ImportContext, file_context: FileContext) -> UsageContext {
        match (import_context, file_context) {
            (ImportContext::Type, _) => UsageContext::Type,
            (ImportContext::Test, _) | (_, FileContext::Test) => UsageContext::Test,
            (_, FileContext::Docs) => UsageContext::Docs,
            (ImportContext::Dev, FileContext::Runtime) | (_, FileContext::Dev) => UsageContext::Dev,
            (ImportContext::Runtime, FileContext::Runtime) => UsageContext::Runtime,
        }
    }

    #[test]
    fn usage_context_follows_the_spec_table() {
        let mut dir_lists: Vec<Vec<&str>> = vec![Vec::new()];
        for depth in 1..=3 {
            let longer: Vec<Vec<&str>> = dir_lists
                .iter()
                .filter(|dirs| dirs.len() == depth - 1)
                .flat_map(|dirs| {
                    DIRS.iter()
                        .map(move |dir| [dirs.as_slice(), &[*dir]].concat())
                })
                .collect();
            dir_lists.extend(longer);
        }
        let mut cases: Vec<(String, FileContext)> = Vec::new();
        for prefix in PREFIXES {
            for dirs in &dir_lists {
                for file in FILES {
                    let path = std::iter::once(*prefix)
                        .filter(|prefix| !prefix.is_empty())
                        .chain(dirs.iter().copied())
                        .chain([*file])
                        .collect::<Vec<_>>()
                        .join("/");
                    cases.push((path, file_context_model(prefix, dirs, file)));
                }
            }
        }
        cases.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(cases.len(), PREFIXES.len() * 400 * FILES.len());

        let mut layout = LayoutInfo::new("src", vec!["acme".to_owned()]);
        layout.members = MEMBERS
            .iter()
            .map(|path| crate::sources::MemberLayout {
                path: (*path).to_owned(),
                layout: LayoutInfo::default(),
            })
            .collect();
        let discovered = |production: bool| {
            let mut sources = empty_sources();
            sources.layout = layout.clone();
            sources.files = cases
                .iter()
                .map(|(path, _)| crate::sources::DiscoveredFile {
                    path: path.clone(),
                    kind: crate::sources::FileKind::Python,
                    context: crate::sources::assign_file_context(path),
                })
                .collect();
            crate::sources::apply_member_context(&mut sources, production);
            sources
        };
        // Every file discovered, `--production` dropping the non-runtime
        // ones (so their context comes from the path again), and none
        // discovered (an import seen in a file outside the inventory).
        let (full, production) = (discovered(false), discovered(true));
        let mut undiscovered = full.clone();
        undiscovered.files.clear();
        for (path, context) in &cases {
            assert_eq!(
                production
                    .files
                    .binary_search_by(|file| file.path.cmp(path))
                    .is_ok(),
                context.is_included_in_production(),
                "production keeps {path}"
            );
            for sources in [&full, &production, &undiscovered] {
                assert_eq!(file_context(path, sources), *context, "{path}");
                for import_context in IMPORT_CONTEXTS {
                    assert_eq!(
                        usage_context_for_import(path, *import_context, sources),
                        usage_model(*import_context, *context),
                        "{path} {import_context:?}"
                    );
                }
            }
        }
    }
}
