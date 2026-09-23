//! CHK003 missing and CHK004 transitive dependency detection.

use std::collections::{HashSet, VecDeque};

use crate::config::{ChokkinConfig, Confidence};
use crate::graph::ModuleOrigin;
use crate::parser::ParseSummary;
use crate::resolver::{ResolvedImport, TransitiveIndex};
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};
use crate::rules::{DependencyRuleContext, RuleContext};

use super::context::{is_directly_declared, usage_context_for_import};
use super::used::DeclaredIndex;

/// Precomputed declaration index for a workspace member manifest.
pub(super) struct WorkspaceDeclaredIndex<'a> {
    pub(super) member_id: &'a str,
    pub(super) declared: DeclaredIndex<'a>,
}

/// Detect missing and transitive-only dependency imports.
pub(super) fn detect_missing_dependencies(
    declared: &DeclaredIndex<'_>,
    dependency: &DependencyRuleContext<'_>,
    reachable: &HashSet<String>,
    has_lockfile: bool,
    workspace_declared: &[WorkspaceDeclaredIndex<'_>],
) -> Vec<IssueCandidate> {
    let DependencyRuleContext {
        rules: context,
        config,
        strict,
    } = *dependency;
    let RuleContext {
        resolution,
        sources,
        ..
    } = *context;
    let optional_imports = collect_optional_imports(context.parse);
    let mut candidates = Vec::new();
    let mut reported = HashSet::new();

    for import in &resolution.imports {
        if import.origin != ModuleOrigin::ThirdParty {
            continue;
        }
        let Some(distribution) = import.distribution.as_ref() else {
            continue;
        };
        if !reachable.contains(&import.file) {
            continue;
        }

        let key = (distribution.clone(), import.file.clone(), import.line);
        if !reported.insert(key) {
            continue;
        }

        let usage = usage_context_for_import(&import.file, import.context, sources);
        let root_declared = declared
            .get(distribution)
            .is_some_and(|deps| is_directly_declared(deps, usage, config));
        let workspace_member = import.workspace_member.as_deref();
        let member_declared = workspace_member.is_some_and(|member_id| {
            workspace_member_declares(workspace_declared, member_id, distribution, usage, config)
        });

        if member_declared
            || (!strict && root_declared)
            || (root_declared && workspace_member.is_none())
        {
            continue;
        }

        if !strict
            && matches!(
                usage,
                super::context::UsageContext::Type
                    | super::context::UsageContext::Test
                    | super::context::UsageContext::Docs
                    | super::context::UsageContext::Dev
            )
        {
            continue;
        }

        if strict
            && root_declared
            && let Some(member_id) = workspace_member
        {
            candidates.push(workspace_missing_candidate(import, distribution, member_id));
            continue;
        }

        // Declared, only in a bucket that does not match this usage context.
        // §10 hands that case to CHK005 alone: it is neither missing (CHK003)
        // nor transitive-only (CHK004).
        if declared.contains_key(distribution) {
            continue;
        }

        if optional_imports.contains(&(import.file.clone(), import.line)) {
            candidates.push(optional_missing_candidate(import, distribution, strict));
            continue;
        }

        if has_lockfile && is_transitive_only(distribution, declared, &resolution.transitive) {
            candidates.push(transitive_candidate(import, distribution));
            continue;
        }

        candidates.push(missing_candidate(import, distribution, has_lockfile));
    }

    candidates
}

fn workspace_member_declares(
    workspace_declared: &[WorkspaceDeclaredIndex<'_>],
    member_id: &str,
    distribution: &str,
    usage: super::context::UsageContext,
    config: &ChokkinConfig,
) -> bool {
    workspace_declared
        .iter()
        .find(|boundary| boundary.member_id == member_id)
        .and_then(|boundary| boundary.declared.get(distribution))
        .is_some_and(|deps| is_directly_declared(deps, usage, config))
}

fn workspace_missing_candidate(
    import: &ResolvedImport,
    distribution: &str,
    member_id: &str,
) -> IssueCandidate {
    IssueCandidate {
        rule: RuleId::Chk003,
        subject: IssueSubject::Import {
            module: import.full_module.clone(),
            file: import.file.clone(),
            line: import.line,
        },
        severity: Severity::Error,
        confidence: Confidence::Certain,
        message: format!(
            "imported {distribution} in {}:{} but workspace member {member_id} does not declare it directly",
            import.file, import.line
        ),
        workspace_member: Some(member_id.to_owned()),
        origins: vec![Origin::Import {
            file: import.file.clone(),
            line: import.line,
            module: import.full_module.clone(),
        }],
        explain: ExplainData {
            summary: format!(
                "{distribution} is declared at the workspace root but not by member {member_id}"
            ),
            details: vec![format!("import at {}:{}", import.file, import.line)],
        },
    }
}

fn optional_missing_candidate(
    import: &ResolvedImport,
    distribution: &str,
    strict: bool,
) -> IssueCandidate {
    let severity = if strict {
        Severity::Warning
    } else {
        Severity::Info
    };
    let (kind, detail) = if import.platform_guarded {
        (
            "platform-guarded import",
            "platform-guarded import — not treated as a hard missing dependency",
        )
    } else {
        (
            "optional try-import",
            "try/except ImportError import — not treated as a hard missing dependency",
        )
    };
    IssueCandidate {
        rule: RuleId::Chk003,
        subject: IssueSubject::Import {
            module: import.full_module.clone(),
            file: import.file.clone(),
            line: import.line,
        },
        severity,
        confidence: Confidence::Likely,
        message: format!("{kind} of {distribution} is not declared in any dependency context"),
        workspace_member: import.workspace_member.clone(),
        origins: vec![Origin::Import {
            file: import.file.clone(),
            line: import.line,
            module: import.full_module.clone(),
        }],
        explain: ExplainData {
            summary: format!("conditional import of {distribution} has no declaration"),
            details: vec![detail.to_owned()],
        },
    }
}

fn transitive_candidate(import: &ResolvedImport, distribution: &str) -> IssueCandidate {
    IssueCandidate {
        rule: RuleId::Chk004,
        subject: IssueSubject::Import {
            module: import.full_module.clone(),
            file: import.file.clone(),
            line: import.line,
        },
        severity: Severity::Error,
        confidence: Confidence::Certain,
        message: format!(
            "imported {distribution} directly but it is only available as a transitive dependency"
        ),
        workspace_member: import.workspace_member.clone(),
        origins: vec![Origin::Import {
            file: import.file.clone(),
            line: import.line,
            module: import.full_module.clone(),
        }],
        explain: ExplainData {
            summary: format!("{distribution} should be declared directly or import removed"),
            details: vec!["resolved via lockfile transitive closure".to_owned()],
        },
    }
}

fn missing_candidate(
    import: &ResolvedImport,
    distribution: &str,
    has_lockfile: bool,
) -> IssueCandidate {
    let lockfile_note = if has_lockfile {
        String::new()
    } else {
        " (no lockfile — transitive check skipped)".to_owned()
    };
    IssueCandidate {
        rule: RuleId::Chk003,
        subject: IssueSubject::Import {
            module: import.full_module.clone(),
            file: import.file.clone(),
            line: import.line,
        },
        severity: Severity::Error,
        confidence: Confidence::Certain,
        message: format!(
            "imported {distribution} in {}:{}{} but not declared in matching dependency context",
            import.file, import.line, lockfile_note
        ),
        workspace_member: import.workspace_member.clone(),
        origins: vec![Origin::Import {
            file: import.file.clone(),
            line: import.line,
            module: import.full_module.clone(),
        }],
        explain: ExplainData {
            summary: format!("{distribution} is imported but not declared"),
            details: vec![format!("import at {}:{}", import.file, import.line)],
        },
    }
}

/// Whether `distribution` is reachable only through the transitive closure of the
/// *other* declared direct dependencies.
#[must_use]
pub(super) fn is_transitive_only(
    distribution: &str,
    declared: &DeclaredIndex<'_>,
    transitive: &TransitiveIndex,
) -> bool {
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();

    // Seeding with `distribution` itself would make any directly declared
    // dependency look transitive before a single edge is walked.
    for deps in declared.values() {
        for dep in deps {
            if dep.name != distribution && visited.insert(dep.name.clone()) {
                queue.push_back(dep.name.clone());
            }
        }
    }

    while let Some(current) = queue.pop_front() {
        if current == distribution {
            return true;
        }
        if let Some(children) = transitive.edges.get(&current) {
            for child in children {
                if visited.insert(child.clone()) {
                    queue.push_back(child.clone());
                }
            }
        }
    }

    false
}

/// Build a set of conditional import locations from parse output.
pub(super) fn collect_optional_imports(parse: &ParseSummary) -> HashSet<(String, u32)> {
    let mut optional = HashSet::new();
    for module in &parse.modules {
        for import in &module.imports {
            if import.optional || import.platform_guarded {
                optional.insert((module.path.clone(), import.line));
            }
        }
    }
    optional
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::default_config;
    use crate::manifest::{DeclaredDependency, DependencyContext, DependencyOrigin};
    use crate::resolver::{ResolutionIndex, TransitiveIndex};

    const FILE: &str = "src/app.py";

    fn declared_dep(name: &str) -> DeclaredDependency {
        declared_dep_in(name, DependencyContext::Runtime)
    }

    fn declared_dep_in(name: &str, context: DependencyContext) -> DeclaredDependency {
        DeclaredDependency {
            name: name.to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(1),
                label: "project.dependencies[0]".to_owned(),
            },
            opaque: false,
        }
    }

    fn runtime_import(distribution: &str) -> ResolvedImport {
        ResolvedImport {
            import_root: distribution.to_owned(),
            full_module: distribution.to_owned(),
            file: FILE.to_owned(),
            workspace_member: None,
            line: 1,
            context: crate::parser::ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            origin: ModuleOrigin::ThirdParty,
            distribution: Some(distribution.to_owned()),
            confidence: crate::resolver::ResolveConfidence::Certain,
        }
    }

    fn sources() -> crate::sources::DiscoveredSources {
        crate::sources::DiscoveredSources {
            root: crate::discovery::ProjectRoot {
                path: std::env::temp_dir(),
                marker: crate::discovery::RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Run the rule on a single runtime import of `distribution` from `src/app.py`.
    fn detect(
        declared: &DeclaredIndex<'_>,
        distribution: &str,
        transitive: TransitiveIndex,
    ) -> Vec<IssueCandidate> {
        let config = default_config();
        let sources = sources();
        let resolution = ResolutionIndex {
            imports: vec![runtime_import(distribution)],
            warnings: Vec::new(),
            transitive,
            binary_resolutions: BTreeMap::new(),
        };
        let graph = crate::graph::ProjectGraph::new(sources.root.clone());
        detect_missing_dependencies(
            declared,
            &DependencyRuleContext {
                rules: &RuleContext {
                    resolution: &resolution,
                    sources: &sources,
                    graph: &graph,
                    reachability: &crate::reachability::ReachabilityReport::empty(),
                    parse: &ParseSummary::empty(),
                },
                config: &config,
                strict: false,
            },
            &HashSet::from([FILE.to_owned()]),
            true,
            &[],
        )
    }

    #[test]
    fn finds_transitive_dependency_in_lockfile_closure() {
        let requests = declared_dep("requests");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        let transitive = TransitiveIndex {
            edges: BTreeMap::from([("requests".to_owned(), vec!["urllib3".to_owned()])]),
        };
        assert!(is_transitive_only("urllib3", &index, &transitive));
        assert!(!is_transitive_only("certifi", &index, &transitive));
    }

    #[test]
    fn declared_distribution_is_never_transitive_only() {
        let requests = declared_dep("requests");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        assert!(!is_transitive_only(
            "requests",
            &index,
            &TransitiveIndex::empty()
        ));
    }

    #[test]
    fn direct_declaration_skips_missing() {
        let requests = declared_dep("requests");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        assert!(detect(&index, "requests", TransitiveIndex::empty()).is_empty());
    }

    /// §10: a dev-group-only dependency used at runtime is CHK005 territory,
    /// so this rule must stay silent instead of adding CHK003/CHK004.
    #[test]
    fn dev_group_only_declaration_is_left_to_misplaced() {
        let pytest = declared_dep_in("pytest", DependencyContext::Group("dev".to_owned()));
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("pytest".to_owned(), vec![&pytest]);

        assert!(detect(&index, "pytest", TransitiveIndex::empty()).is_empty());

        let transitive = TransitiveIndex {
            edges: BTreeMap::from([("pytest".to_owned(), vec!["pluggy".to_owned()])]),
        };
        assert!(detect(&index, "pytest", transitive).is_empty());
    }
}
