//! CHK003 missing and CHK004 transitive dependency detection.

use std::collections::{HashSet, VecDeque};

use crate::config::Confidence;
use crate::graph::ModuleOrigin;
use crate::manifest::{DeclaredDependency, LockfileGraph};
use crate::parser::ParseSummary;
use crate::resolver::ResolvedImport;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};
use crate::rules::{DependencyRuleContext, RuleContext};

use super::context::{is_directly_declared, usage_context_for_import};
use super::used::DeclaredIndex;

/// Precomputed declaration index for a workspace member manifest.
pub(super) struct WorkspaceDeclaredIndex<'a> {
    pub(super) member_id: &'a str,
    pub(super) declared: DeclaredIndex<'a>,
    /// The member's own lockfile, when it has one (langchain's
    /// `libs/core/uv.lock`, #653).
    pub(super) lockfile: Option<&'a LockfileGraph>,
}

/// A lockfile and the declarations its edges are walked from.
type LockScope<'i, 'a> = (&'i DeclaredIndex<'a>, &'i LockfileGraph);

/// Detect missing and transitive-only dependency imports.
pub(super) fn detect_missing_dependencies(
    declared: &DeclaredIndex<'_>,
    dependency: &DependencyRuleContext<'_>,
    reachable: &HashSet<&str>,
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
        if !reachable.contains(import.file.as_str()) {
            continue;
        }

        if !reported.insert((distribution.clone(), import.file.clone(), import.line)) {
            continue;
        }

        let usage = usage_context_for_import(&import.file, import.context, sources);
        let workspace_member = import.workspace_member.as_deref();
        let boundary = workspace_member.and_then(|member_id| {
            workspace_declared
                .iter()
                .find(|boundary| boundary.member_id == member_id)
        });
        let member_entry = boundary
            .and_then(|boundary| boundary.declared.get(distribution))
            .map(Vec::as_slice);
        let root_entry = declared.get(distribution).map(Vec::as_slice);
        let member_declared =
            member_entry.is_some_and(|deps| is_directly_declared(deps, usage, config));
        let root_declared =
            root_entry.is_some_and(|deps| is_directly_declared(deps, usage, config));

        if member_declared {
            continue;
        }

        if !strict && usage != super::context::UsageContext::Runtime {
            continue;
        }

        if strict
            && root_declared
            && member_entry.is_none()
            && let Some(member_id) = workspace_member
        {
            candidates.push(workspace_missing_candidate(import, distribution, member_id));
            continue;
        }

        // Declared in any bucket, including the root declaration a non-strict
        // or non-member import relies on. A bucket that does not match this
        // usage context is CHK005's alone (§10), not CHK003 or CHK004.
        if governing_declarations(
            declared,
            workspace_declared,
            workspace_member,
            distribution,
            strict,
        )
        .is_some()
        {
            continue;
        }

        // A member's own lockfile is walked from the member's declarations
        // (#653); the root's lockfile still counts from the root's.
        let scopes = [
            boundary.and_then(|boundary| Some((&boundary.declared, boundary.lockfile?))),
            has_lockfile.then_some((declared, &resolution.transitive)),
        ];
        let optional = optional_imports.contains(&(import.file.clone(), import.line));
        candidates.push(undeclared_candidate(
            import,
            distribution,
            &scopes,
            optional,
            strict,
        ));
    }

    candidates
}

/// §10: CHK004 when a lockfile accounts for the import, CHK003 otherwise.
fn undeclared_candidate(
    import: &ResolvedImport,
    distribution: &str,
    scopes: &[Option<LockScope<'_, '_>>],
    optional: bool,
    strict: bool,
) -> IssueCandidate {
    let evidence = lock_evidence(distribution, scopes);
    // A lock edge proves a transitive dependency, so wrapping the import in
    // `try:` must not hide CHK004; a lock entry without an edge is too weak
    // to override the optional relaxation (#504).
    if optional && evidence != Some(LockEvidence::TransitiveEdge) {
        return optional_missing_candidate(import, distribution, strict);
    }
    let candidate = match evidence {
        Some(evidence) => transitive_candidate(import, distribution, evidence),
        None => missing_candidate(import, distribution, scopes.iter().any(Option::is_some)),
    };
    demote_optional_candidate(import, optional, candidate)
}

/// How the lockfile accounts for an undeclared import (CHK004 evidence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockEvidence {
    /// Reachable through the lock edges of a declared dependency.
    TransitiveEdge,
    /// Listed in the lockfile, but no edge from a declared dependency reaches
    /// it (stale lock, or a format such as `pylock.toml` without edges).
    LockedOnly,
}

/// The strongest CHK004 evidence any lockfile gives.
fn lock_evidence(distribution: &str, scopes: &[Option<LockScope<'_, '_>>]) -> Option<LockEvidence> {
    let mut scopes = scopes.iter().flatten();
    if scopes
        .clone()
        .any(|(declared, lock)| is_transitive_only(distribution, declared, lock))
    {
        return Some(LockEvidence::TransitiveEdge);
    }
    scopes
        .any(|(_, lock)| lock.edges.contains_key(distribution))
        .then_some(LockEvidence::LockedOnly)
}

/// Declarations that decide between CHK003/CHK004/CHK005 for one import.
/// Under `--strict` a workspace member's own entry wins and the root is the
/// fallback; otherwise only the root counts. Missing and misplaced detection
/// share this lookup so that at most one of them fires per import.
pub(super) fn governing_declarations<'i, 'a>(
    declared: &'i DeclaredIndex<'a>,
    workspace_declared: &'i [WorkspaceDeclaredIndex<'a>],
    workspace_member: Option<&str>,
    distribution: &str,
    strict: bool,
) -> Option<&'i [&'a DeclaredDependency]> {
    workspace_member
        .filter(|_| strict)
        .and_then(|member_id| member_declarations(workspace_declared, member_id, distribution))
        .or_else(|| declared.get(distribution).map(Vec::as_slice))
}

pub(super) fn member_declarations<'i, 'a>(
    workspace_declared: &'i [WorkspaceDeclaredIndex<'a>],
    member_id: &str,
    distribution: &str,
) -> Option<&'i [&'a DeclaredDependency]> {
    workspace_declared
        .iter()
        .find(|boundary| boundary.member_id == member_id)
        .and_then(|boundary| boundary.declared.get(distribution))
        .map(Vec::as_slice)
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
            distribution: Some(distribution.to_owned()),
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
    let kind = if import.platform_guarded {
        "platform-guarded import"
    } else {
        "optional try-import"
    };
    let detail = conditional_import_detail(import);
    IssueCandidate {
        rule: RuleId::Chk003,
        subject: IssueSubject::Import {
            module: import.full_module.clone(),
            file: import.file.clone(),
            line: import.line,
            distribution: Some(distribution.to_owned()),
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

/// The lock edge keeps the rule CHK004, but the import still runs only when
/// the package is available, so it is not an error (#582).
fn demote_optional_candidate(
    import: &ResolvedImport,
    optional: bool,
    mut candidate: IssueCandidate,
) -> IssueCandidate {
    if !optional {
        return candidate;
    }
    candidate.severity = Severity::Warning;
    candidate
        .explain
        .details
        .push(conditional_import_detail(import).to_owned());
    candidate
}

fn conditional_import_detail(import: &ResolvedImport) -> &'static str {
    if import.platform_guarded {
        "platform-guarded import — not treated as a hard missing dependency"
    } else {
        "try/except ImportError import — not treated as a hard missing dependency"
    }
}

fn transitive_candidate(
    import: &ResolvedImport,
    distribution: &str,
    evidence: LockEvidence,
) -> IssueCandidate {
    let (confidence, message, detail) = match evidence {
        LockEvidence::TransitiveEdge => (
            Confidence::Certain,
            format!(
                "imported {distribution} directly but it is only available as a transitive dependency"
            ),
            "resolved via lockfile transitive closure",
        ),
        // Without an edge the lock may be stale, so the evidence is weaker.
        LockEvidence::LockedOnly => (
            Confidence::Likely,
            format!(
                "imported {distribution} directly but it is only pinned in the lockfile, not declared in the manifest"
            ),
            "listed in the lockfile but not reachable from any declared dependency",
        ),
    };
    IssueCandidate {
        rule: RuleId::Chk004,
        subject: IssueSubject::Import {
            module: import.full_module.clone(),
            file: import.file.clone(),
            line: import.line,
            distribution: Some(distribution.to_owned()),
        },
        severity: Severity::Error,
        confidence,
        message,
        workspace_member: import.workspace_member.clone(),
        origins: vec![Origin::Import {
            file: import.file.clone(),
            line: import.line,
            module: import.full_module.clone(),
        }],
        explain: ExplainData {
            summary: format!("{distribution} should be declared directly or import removed"),
            details: vec![detail.to_owned()],
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
            distribution: Some(distribution.to_owned()),
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
    transitive: &LockfileGraph,
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
        let flags = module
            .imports
            .iter()
            .map(|import| (import.line, import.optional || import.platform_guarded))
            .chain(
                module
                    .dynamic_imports
                    .iter()
                    .map(|dynamic| (dynamic.line, dynamic.optional || dynamic.platform_guarded)),
            );
        for (line, conditional) in flags {
            if conditional {
                optional.insert((module.path.clone(), line));
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
    use crate::resolver::ResolutionIndex;

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
            included_via: Vec::new(),
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
            },
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Src,
                package_root: "src".to_owned(),
                packages: vec!["acme".to_owned()],
                ..Default::default()
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
        transitive: LockfileGraph,
    ) -> Vec<IssueCandidate> {
        detect_with(
            declared,
            runtime_import(distribution),
            transitive,
            false,
            &[],
        )
    }

    fn detect_with(
        declared: &DeclaredIndex<'_>,
        import: ResolvedImport,
        transitive: LockfileGraph,
        strict: bool,
        workspace_declared: &[WorkspaceDeclaredIndex<'_>],
    ) -> Vec<IssueCandidate> {
        detect_with_lock(
            declared,
            import,
            Some(transitive),
            strict,
            workspace_declared,
        )
    }

    /// `lockfile: None` runs the rule as if the root has no lockfile.
    fn detect_with_lock(
        declared: &DeclaredIndex<'_>,
        import: ResolvedImport,
        lockfile: Option<LockfileGraph>,
        strict: bool,
        workspace_declared: &[WorkspaceDeclaredIndex<'_>],
    ) -> Vec<IssueCandidate> {
        let has_lockfile = lockfile.is_some();
        let config = default_config();
        let sources = sources();
        let resolution = ResolutionIndex {
            imports: vec![import],
            warnings: Vec::new(),
            transitive: lockfile.unwrap_or_default(),
            binary_resolutions: BTreeMap::new(),
            pytest_plugin_distributions: std::collections::BTreeSet::new(),
        };
        let graph = crate::graph::ProjectGraph::new(sources.root.clone());
        let parse = parse_for(&resolution.imports);
        detect_missing_dependencies(
            declared,
            &DependencyRuleContext {
                rules: &RuleContext {
                    resolution: &resolution,
                    sources: &sources,
                    graph: &graph,
                    reachability: &crate::reachability::ReachabilityReport::default(),
                    parse: &parse,
                },
                config: &config,
                strict,
            },
            &HashSet::from([FILE]),
            has_lockfile,
            workspace_declared,
        )
    }

    /// Parse output carrying each import's `optional` / `platform_guarded` flags.
    fn parse_for(imports: &[ResolvedImport]) -> ParseSummary {
        let imports = imports
            .iter()
            .map(|import| crate::parser::ImportRef {
                module: import.full_module.clone(),
                name: None,
                alias: None,
                line: import.line,
                kind: crate::parser::ImportKind::Import,
                context: import.context,
                optional: import.optional,
                platform_guarded: import.platform_guarded,
                deferred: false,
                relative_level: 0,
            })
            .collect();
        ParseSummary {
            modules: vec![crate::parser::ParsedModule {
                path: FILE.to_owned(),
                imports,
                ..crate::parser::ParsedModule::default()
            }],
        }
    }

    /// #582: a lock edge keeps an optional import on CHK004 (#504), but as a
    /// warning; without the edge it stays the conditional CHK003.
    #[test]
    fn optional_import_with_lock_edge_is_chk004_warning() {
        let requests = declared_dep("requests");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        let transitive = || LockfileGraph {
            edges: BTreeMap::from([
                ("requests".to_owned(), vec!["urllib3".to_owned()]),
                ("urllib3".to_owned(), Vec::new()),
            ]),
            ..LockfileGraph::default()
        };
        let optional = |distribution| ResolvedImport {
            optional: true,
            ..runtime_import(distribution)
        };

        for strict in [false, true] {
            let edge = detect_with(&index, optional("urllib3"), transitive(), strict, &[]);
            assert_eq!(edge.len(), 1);
            assert_eq!(edge[0].rule, RuleId::Chk004);
            assert_eq!(edge[0].severity, Severity::Warning);
            assert_eq!(edge[0].confidence, Confidence::Certain);
            assert!(
                edge[0]
                    .explain
                    .details
                    .iter()
                    .any(|detail| detail.starts_with("try/except ImportError import")),
                "{:?}",
                edge[0].explain.details
            );
        }

        let plain = detect(&index, "urllib3", transitive());
        assert_eq!(plain[0].severity, Severity::Error);

        let no_edge = detect_with(&index, optional("certifi"), transitive(), false, &[]);
        assert_eq!(no_edge[0].rule, RuleId::Chk003);
        assert_eq!(no_edge[0].severity, Severity::Info);
        assert_eq!(no_edge[0].confidence, Confidence::Likely);
    }

    #[test]
    fn finds_transitive_dependency_in_lockfile_closure() {
        let requests = declared_dep("requests");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        let transitive = LockfileGraph {
            edges: BTreeMap::from([("requests".to_owned(), vec!["urllib3".to_owned()])]),
            ..LockfileGraph::default()
        };
        assert!(is_transitive_only("urllib3", &index, &transitive));
        assert!(!is_transitive_only("certifi", &index, &transitive));
    }

    #[test]
    fn lock_evidence_separates_transitive_edge_from_locked_only() {
        let requests = declared_dep("requests");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        let transitive = || LockfileGraph {
            edges: BTreeMap::from([
                ("requests".to_owned(), vec!["urllib3".to_owned()]),
                ("urllib3".to_owned(), Vec::new()),
                ("pyyaml".to_owned(), Vec::new()),
            ]),
            ..LockfileGraph::default()
        };

        let edge = detect(&index, "urllib3", transitive());
        assert_eq!(edge.len(), 1);
        assert_eq!(edge[0].rule, RuleId::Chk004);
        assert_eq!(edge[0].confidence, Confidence::Certain);
        assert!(edge[0].message.contains("transitive dependency"));

        let locked = detect(&index, "pyyaml", transitive());
        assert_eq!(locked.len(), 1);
        assert_eq!(locked[0].rule, RuleId::Chk004);
        assert_eq!(locked[0].confidence, Confidence::Likely);
        assert!(locked[0].message.contains("only pinned in the lockfile"));
        assert_ne!(edge[0].explain.details, locked[0].explain.details);

        let absent = detect(&index, "certifi", transitive());
        assert_eq!(absent.len(), 1);
        assert_eq!(absent[0].rule, RuleId::Chk003);
    }

    #[test]
    fn declared_distribution_is_never_transitive_only() {
        let requests = declared_dep("requests");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        assert!(!is_transitive_only(
            "requests",
            &index,
            &LockfileGraph::default()
        ));
    }

    #[test]
    fn direct_declaration_skips_missing() {
        let requests = declared_dep("requests");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        assert_eq!(detect(&index, "requests", LockfileGraph::default()), []);
    }

    /// §10: a dev-group-only dependency used at runtime is CHK005 territory,
    /// so this rule must stay silent instead of adding CHK003/CHK004.
    #[test]
    fn dev_group_only_declaration_is_left_to_misplaced() {
        let pytest = declared_dep_in("pytest", DependencyContext::Group("dev".to_owned()));
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("pytest".to_owned(), vec![&pytest]);

        assert_eq!(detect(&index, "pytest", LockfileGraph::default()), []);

        let transitive = LockfileGraph {
            edges: BTreeMap::from([("pytest".to_owned(), vec!["pluggy".to_owned()])]),
            ..LockfileGraph::default()
        };
        assert_eq!(detect(&index, "pytest", transitive), []);
    }

    #[test]
    fn workspace_member_import_accepts_member_or_root_declaration() {
        let requests = declared_dep("requests");
        let with_requests = || BTreeMap::from([("requests".to_owned(), vec![&requests])]);
        let member_import = || ResolvedImport {
            workspace_member: Some("api".to_owned()),
            ..runtime_import("requests")
        };
        let member_only = [WorkspaceDeclaredIndex {
            member_id: "api",
            declared: with_requests(),
            lockfile: None,
        }];
        let no_member = [WorkspaceDeclaredIndex {
            member_id: "api",
            declared: BTreeMap::new(),
            lockfile: None,
        }];

        for strict in [false, true] {
            let found = detect_with(
                &BTreeMap::new(),
                member_import(),
                LockfileGraph::default(),
                strict,
                &member_only,
            );
            assert!(found.is_empty(), "strict={strict}: {found:?}");
        }

        let root_only = detect_with(
            &with_requests(),
            member_import(),
            LockfileGraph::default(),
            false,
            &no_member,
        );
        assert!(root_only.is_empty(), "{root_only:?}");
    }

    /// #653: a member's own lockfile decides CHK004 for its imports when the
    /// root has none, walked from the member's declarations.
    #[test]
    fn member_lockfile_decides_transitive_for_member_import() {
        let pydantic = declared_dep("pydantic");
        let member_lock = LockfileGraph {
            edges: BTreeMap::from([
                ("pydantic".to_owned(), vec!["pydantic-core".to_owned()]),
                ("pydantic-core".to_owned(), Vec::new()),
            ]),
            ..LockfileGraph::default()
        };
        let member_import = |member: &str| ResolvedImport {
            workspace_member: Some(member.to_owned()),
            ..runtime_import("pydantic-core")
        };
        let members = [
            WorkspaceDeclaredIndex {
                member_id: "core",
                declared: BTreeMap::from([("pydantic".to_owned(), vec![&pydantic])]),
                lockfile: Some(&member_lock),
            },
            WorkspaceDeclaredIndex {
                member_id: "unlocked",
                declared: BTreeMap::from([("pydantic".to_owned(), vec![&pydantic])]),
                lockfile: None,
            },
        ];
        let run = |member: &str| {
            detect_with_lock(
                &BTreeMap::new(),
                member_import(member),
                None,
                false,
                &members,
            )
        };

        let locked = run("core");
        assert_eq!(locked.len(), 1, "{locked:?}");
        assert_eq!(locked[0].rule, RuleId::Chk004);
        assert_eq!(locked[0].confidence, Confidence::Certain);

        let unlocked = run("unlocked");
        assert_eq!(unlocked.len(), 1, "{unlocked:?}");
        assert_eq!(unlocked[0].rule, RuleId::Chk003);
        assert!(unlocked[0].message.contains("no lockfile"));

        let optional = detect_with_lock(
            &BTreeMap::new(),
            ResolvedImport {
                optional: true,
                ..member_import("core")
            },
            None,
            false,
            &members,
        );
        assert_eq!(optional.len(), 1, "{optional:?}");
        assert_eq!(optional[0].rule, RuleId::Chk004);
        assert_eq!(optional[0].severity, Severity::Warning);
    }

    /// #653: a member lockfile adds evidence; the root's lock edges from the
    /// root's declarations still count.
    #[test]
    fn root_lockfile_still_counts_beside_member_lockfile() {
        let requests = declared_dep("requests");
        let root_lock = LockfileGraph {
            edges: BTreeMap::from([
                ("requests".to_owned(), vec!["urllib3".to_owned()]),
                ("urllib3".to_owned(), Vec::new()),
            ]),
            ..LockfileGraph::default()
        };
        let member_lock = LockfileGraph::default();
        let members = [WorkspaceDeclaredIndex {
            member_id: "core",
            declared: BTreeMap::new(),
            lockfile: Some(&member_lock),
        }];

        let found = detect_with(
            &BTreeMap::from([("requests".to_owned(), vec![&requests])]),
            ResolvedImport {
                workspace_member: Some("core".to_owned()),
                ..runtime_import("urllib3")
            },
            root_lock,
            false,
            &members,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rule, RuleId::Chk004);
        assert_eq!(found[0].confidence, Confidence::Certain);
    }

    #[test]
    fn non_runtime_usage_is_reported_only_under_strict() {
        let test_import = || ResolvedImport {
            context: crate::parser::ImportContext::Test,
            ..runtime_import("pytest_mock")
        };
        let index: DeclaredIndex<'_> = BTreeMap::new();

        let relaxed = detect_with(&index, test_import(), LockfileGraph::default(), false, &[]);
        assert_eq!(relaxed, []);

        let strict = detect_with(&index, test_import(), LockfileGraph::default(), true, &[]);
        assert_eq!(strict.len(), 1);
        assert_eq!(strict[0].rule, RuleId::Chk003);
    }

    #[test]
    fn optional_and_platform_guarded_imports_are_both_collected() {
        let import = |line, optional, platform_guarded| crate::parser::ImportRef {
            module: "pkg".to_owned(),
            name: None,
            alias: None,
            line,
            kind: crate::parser::ImportKind::Import,
            context: crate::parser::ImportContext::Runtime,
            optional,
            platform_guarded,
            deferred: false,
            relative_level: 0,
        };
        let dynamic = |line, optional, platform_guarded| crate::parser::DynamicImport {
            module: "pkg".to_owned(),
            line,
            optional,
            platform_guarded,
            deferred: false,
        };
        let parse = ParseSummary {
            modules: vec![crate::parser::ParsedModule {
                path: FILE.to_owned(),
                imports: vec![
                    import(1, false, false),
                    import(2, true, false),
                    import(3, false, true),
                    import(4, true, true),
                ],
                dynamic_imports: vec![
                    dynamic(5, false, false),
                    dynamic(6, true, false),
                    dynamic(7, false, true),
                ],
                ..crate::parser::ParsedModule::default()
            }],
        };
        assert_eq!(
            collect_optional_imports(&parse),
            HashSet::from([2, 3, 4, 6, 7].map(|line| (FILE.to_owned(), line)))
        );
    }
}
