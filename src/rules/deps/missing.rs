//! CHK003 missing and CHK004 transitive dependency detection.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ptr;

use crate::config::{ChokkinConfig, Confidence};
use crate::graph::ModuleOrigin;
use crate::manifest::{DeclaredDependency, LockfileGraph};
use crate::parser::ParseSummary;
use crate::resolver::ResolvedImport;
use crate::rules::types::{
    ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity, import_evidence,
    imported_in,
};
use crate::rules::{DependencyRuleContext, RuleContext};

use super::context::{UsageContext, is_directly_declared, usage_context_for_import};
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
        ..
    } = *dependency;
    let RuleContext {
        resolution,
        sources,
        ..
    } = *context;
    let optional_imports = collect_optional_imports(context.parse);
    let mut candidates = Vec::new();
    let mut reported = HashSet::new();
    let mut closures = LockClosures::default();

    for import in &resolution.imports {
        // An import no allowed interpreter runs needs no declaration (#1027).
        if import.origin != ModuleOrigin::ThirdParty || import.never_runs {
            continue;
        }
        let Some(distribution) = import.distribution.as_ref() else {
            continue;
        };
        if !reachable.contains(import.file.as_str()) {
            continue;
        }

        if !reported.insert(report_key(distribution, import)) {
            continue;
        }

        let usage = usage_context_for_import(&import.file, import.context, sources);
        let workspace_member = import.workspace_member.as_deref();
        let declarations = ImportDeclarations::lookup(
            declared,
            workspace_declared,
            workspace_member,
            distribution,
        );

        if declarations.member_satisfies(usage, config) {
            continue;
        }

        if !strict && usage != UsageContext::Runtime {
            continue;
        }

        if strict
            && declarations.member.is_none()
            && declarations
                .root
                .is_some_and(|deps| is_directly_declared(deps, usage, config))
            && let Some(member_id) = workspace_member
        {
            candidates.push(workspace_missing_candidate(import, distribution, member_id));
            continue;
        }

        // Declared in any bucket, including the root declaration a non-strict
        // or non-member import relies on. A bucket that does not match this
        // usage context is CHK005's alone (§10), not CHK003 or CHK004.
        if declarations.governing(strict).is_some() {
            continue;
        }

        // Lock edges are walked from the importing member's declarations: a
        // package only the root's reach (a root extra) may be absent (#649).
        let boundary = declarations.boundary;
        let seeds = boundary.map_or(declared, |boundary| &boundary.declared);
        let scopes = [
            boundary.and_then(|boundary| Some((seeds, boundary.lockfile?))),
            has_lockfile.then_some((seeds, &resolution.transitive)),
        ];
        let optional = is_optional(&optional_imports, import);
        candidates.push(undeclared_candidate(
            import,
            distribution,
            &scopes,
            &mut closures,
            optional,
            strict,
        ));
    }

    candidates
}

/// §10: CHK004 when a lockfile accounts for the import, CHK003 otherwise.
#[expect(
    clippy::too_many_arguments,
    reason = "one import's fields plus the lock closure cache shared across imports"
)]
fn undeclared_candidate<'i>(
    import: &ResolvedImport,
    distribution: &str,
    scopes: &[Option<LockScope<'i, '_>>],
    closures: &mut LockClosures<'i>,
    optional: bool,
    strict: bool,
) -> IssueCandidate {
    let evidence = lock_evidence(distribution, scopes, closures);
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
fn lock_evidence<'i>(
    distribution: &str,
    scopes: &[Option<LockScope<'i, '_>>],
    closures: &mut LockClosures<'i>,
) -> Option<LockEvidence> {
    let mut scopes = scopes.iter().flatten();
    if scopes
        .clone()
        .any(|&(declared, lock)| closures.is_transitive_only(distribution, declared, lock))
    {
        return Some(LockEvidence::TransitiveEdge);
    }
    scopes
        .any(|(_, lock)| lock.edges.contains_key(distribution))
        .then_some(LockEvidence::LockedOnly)
}

/// The declarations one import is checked against: its workspace member's
/// and the root's. Missing and misplaced detection share this lookup so that
/// at most one of them fires per import.
pub(super) struct ImportDeclarations<'i, 'a> {
    /// The importing member's declarations, when the import is in a member.
    boundary: Option<&'i WorkspaceDeclaredIndex<'a>>,
    /// The member's entries for the distribution.
    member: Option<&'i [&'a DeclaredDependency]>,
    /// The root's entries for the distribution.
    root: Option<&'i [&'a DeclaredDependency]>,
}

impl<'i, 'a> ImportDeclarations<'i, 'a> {
    pub(super) fn lookup(
        declared: &'i DeclaredIndex<'a>,
        workspace_declared: &'i [WorkspaceDeclaredIndex<'a>],
        workspace_member: Option<&str>,
        distribution: &str,
    ) -> Self {
        let boundary = workspace_member.and_then(|member_id| {
            workspace_declared
                .iter()
                .find(|boundary| boundary.member_id == member_id)
        });
        Self {
            boundary,
            member: boundary
                .and_then(|boundary| boundary.declared.get(distribution))
                .map(Vec::as_slice),
            root: declared.get(distribution).map(Vec::as_slice),
        }
    }

    /// A member that declares it directly for this usage is satisfied, even
    /// when the root lists it only in a dev group.
    pub(super) fn member_satisfies(&self, usage: UsageContext, config: &ChokkinConfig) -> bool {
        self.member
            .is_some_and(|deps| is_directly_declared(deps, usage, config))
    }

    /// Declarations that decide between CHK003/CHK004/CHK005. Under
    /// `--strict` a workspace member's own entry wins and the root is the
    /// fallback; otherwise only the root counts.
    pub(super) fn governing(&self, strict: bool) -> Option<&'i [&'a DeclaredDependency]> {
        self.member.filter(|_| strict).or(self.root)
    }
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
            "{} but workspace member {member_id} does not declare it directly",
            imported_in(distribution, import)
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
            details: vec![import_evidence(import)],
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
            "{}{lockfile_note} but not declared in matching dependency context",
            imported_in(distribution, import)
        ),
        workspace_member: import.workspace_member.clone(),
        origins: vec![Origin::Import {
            file: import.file.clone(),
            line: import.line,
            module: import.full_module.clone(),
        }],
        explain: ExplainData {
            summary: format!("{distribution} is imported but not declared"),
            details: vec![import_evidence(import)],
        },
    }
}

/// Lock closures of each declaration set, walked once per scope instead of
/// once per import (#972). Keyed by address: a scope's declarations and
/// lockfile are borrowed for the whole detection pass.
#[derive(Default)]
struct LockClosures<'i> {
    by_scope: HashMap<(usize, usize), HashSet<&'i str>>,
}

impl<'i> LockClosures<'i> {
    fn is_transitive_only(
        &mut self,
        distribution: &str,
        declared: &'i DeclaredIndex<'_>,
        lock: &'i LockfileGraph,
    ) -> bool {
        // A seed's own closure excludes it, so it cannot share the cache.
        if declared
            .values()
            .flatten()
            .any(|dep| dep.name == distribution)
        {
            return is_transitive_only(distribution, declared, lock);
        }
        let key = (ptr::from_ref(declared).addr(), ptr::from_ref(lock).addr());
        self.by_scope
            .entry(key)
            .or_insert_with(|| lock_closure(declared, lock, None))
            .contains(distribution)
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
    // Seeding with `distribution` itself would make any directly declared
    // dependency look transitive before a single edge is walked.
    lock_closure(declared, transitive, Some(distribution)).contains(distribution)
}

/// Every name the lock edges reach from the declarations, `except` left out
/// of the seeds (but not of what the others reach).
fn lock_closure<'g>(
    declared: &'g DeclaredIndex<'_>,
    lock: &'g LockfileGraph,
    except: Option<&str>,
) -> HashSet<&'g str> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    for dep in declared.values().flatten() {
        let name = dep.name.as_str();
        if Some(name) != except && visited.insert(name) {
            queue.push_back(name);
        }
    }
    while let Some(current) = queue.pop_front() {
        for child in lock.edges.get(current).into_iter().flatten() {
            if visited.insert(child.as_str()) {
                queue.push_back(child.as_str());
            }
        }
    }
    visited
}

/// One issue per distribution and `import` statement; a config reference keys
/// without its line, so a setting's entries from one distribution stay one
/// issue as before they had lines (#830).
fn report_key(distribution: &str, import: &ResolvedImport) -> (String, String, Option<u32>) {
    (
        distribution.to_owned(),
        import.file.clone(),
        import.statement_line(),
    )
}

/// Whether `import` is a statement in [`collect_optional_imports`]; a config
/// reference is no statement, so it never is (#830).
fn is_optional(optional_imports: &HashSet<(String, u32)>, import: &ResolvedImport) -> bool {
    import
        .statement_line()
        .is_some_and(|line| optional_imports.contains(&(import.file.clone(), line)))
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
    use std::sync::Arc;

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
            unportable_key: None,
        }
    }

    fn runtime_import(distribution: &str) -> ResolvedImport {
        ResolvedImport {
            import_root: distribution.to_owned(),
            full_module: distribution.to_owned(),
            file: FILE.to_owned(),
            workspace_member: None,
            line: Some(1),
            reference: None,
            context: crate::parser::ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            never_runs: false,
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
            layout: crate::sources::LayoutInfo::new("src", vec!["acme".to_owned()]),
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
            imports: vec![Arc::new(import)],
            string_refs: Vec::new(),
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
                    surface: None,
                },
                config: &config,
                strict,
                build_scripts: &BTreeMap::new(),
                local: &crate::sources::LocalTrees::default(),
            },
            &HashSet::from([FILE]),
            has_lockfile,
            workspace_declared,
        )
    }

    /// Parse output carrying each import's `optional` / `platform_guarded` flags.
    fn parse_for(imports: &[Arc<ResolvedImport>]) -> ParseSummary {
        let imports = imports
            .iter()
            .map(|import| crate::parser::ImportRef {
                module: import.full_module.clone(),
                name: None,
                alias: None,
                line: import.line.unwrap_or_default(),
                kind: crate::parser::ImportKind::Import,
                context: import.context,
                optional: import.optional,
                platform_guarded: import.platform_guarded,
                deferred: false,
                relative_level: 0,
                fallback_for: Vec::new(),
                version_minors: None,
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
    fn cached_closure_agrees_with_a_fresh_walk() {
        let requests = declared_dep("requests");
        let urllib3 = declared_dep("urllib3");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        index.insert("urllib3".to_owned(), vec![&urllib3]);
        let lock = LockfileGraph {
            edges: BTreeMap::from([
                (
                    "requests".to_owned(),
                    vec!["urllib3".to_owned(), "idna".to_owned()],
                ),
                ("urllib3".to_owned(), vec!["brotli".to_owned()]),
                ("idna".to_owned(), Vec::new()),
                ("certifi".to_owned(), Vec::new()),
            ]),
            ..LockfileGraph::default()
        };
        let mut closures = LockClosures::default();
        // `urllib3` and `requests` are seeds: their answer excludes themselves.
        for name in ["idna", "brotli", "urllib3", "requests", "certifi", "absent"] {
            assert_eq!(
                closures.is_transitive_only(name, &index, &lock),
                is_transitive_only(name, &index, &lock),
                "{name}"
            );
        }
        assert_eq!(closures.by_scope.len(), 1);
    }

    /// #972: 300k lock packages and 200k undeclared imports took over 240 s
    /// when every import walked the whole lock. Run with `--ignored --release`.
    #[test]
    #[ignore = "stress: run manually with --release"]
    fn large_lock_with_many_imports_walks_the_closure_once() {
        let root = declared_dep("pkg-0");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("pkg-0".to_owned(), vec![&root]);
        let packages = 300_000;
        let lock = LockfileGraph {
            edges: (0..packages)
                .map(|i| (format!("pkg-{i}"), vec![format!("pkg-{}", i + 1)]))
                .collect(),
            ..LockfileGraph::default()
        };
        let started = std::time::Instant::now();
        let mut closures = LockClosures::default();
        let reached = (0..200_000)
            .filter(|i| closures.is_transitive_only(&format!("pkg-{}", i * 2 + 1), &index, &lock))
            .count();
        assert_eq!(reached, 150_000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
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

    /// CHK003-CHK005 read one lookup: the member's runtime declaration
    /// satisfies the import, and `--strict` lets the member's entry govern
    /// over the root's.
    #[test]
    fn import_declarations_prefer_the_member_under_strict() {
        let config = default_config();
        let root_dev = declared_dep_in("requests", DependencyContext::Group("dev".to_owned()));
        let member_runtime = declared_dep("requests");
        let root = BTreeMap::from([("requests".to_owned(), vec![&root_dev])]);
        let members = [WorkspaceDeclaredIndex {
            member_id: "api",
            declared: BTreeMap::from([("requests".to_owned(), vec![&member_runtime])]),
            lockfile: None,
        }];
        let lookup =
            |member: Option<&str>| ImportDeclarations::lookup(&root, &members, member, "requests");
        let runtime = UsageContext::Runtime;

        let in_member = lookup(Some("api"));
        assert!(in_member.member_satisfies(runtime, &config));
        assert_eq!(in_member.governing(true), Some(&[&member_runtime][..]));
        assert_eq!(in_member.governing(false), Some(&[&root_dev][..]));

        let at_root = lookup(None);
        assert!(!at_root.member_satisfies(runtime, &config));
        assert_eq!(at_root.governing(true), Some(&[&root_dev][..]));
        assert!(lookup(Some("web")).governing(true).is_some());
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

    /// #649: the root's lockfile is walked from the importing member's
    /// declarations; an edge reached only through the root's (a root extra in
    /// fastmcp) is not evidence the member installs the package.
    #[test]
    fn root_lockfile_is_walked_from_member_declarations() {
        let requests = declared_dep("requests");
        let tasks = declared_dep_in(
            "fastmcp-tasks",
            DependencyContext::OptionalExtra("tasks".to_owned()),
        );
        let root_lock = || LockfileGraph {
            edges: BTreeMap::from([
                ("requests".to_owned(), vec!["urllib3".to_owned()]),
                ("urllib3".to_owned(), Vec::new()),
                ("fastmcp-tasks".to_owned(), vec!["pydocket".to_owned()]),
                ("pydocket".to_owned(), Vec::new()),
            ]),
            ..LockfileGraph::default()
        };
        let root = BTreeMap::from([("fastmcp-tasks".to_owned(), vec![&tasks])]);
        let members = [WorkspaceDeclaredIndex {
            member_id: "slim",
            declared: BTreeMap::from([("requests".to_owned(), vec![&requests])]),
            lockfile: None,
        }];
        let import = |distribution, member: Option<&str>, optional| ResolvedImport {
            workspace_member: member.map(str::to_owned),
            optional,
            ..runtime_import(distribution)
        };
        let run = |import| detect_with(&root, import, root_lock(), false, &members);

        let member_edge = run(import("urllib3", Some("slim"), false));
        assert_eq!(member_edge.len(), 1, "{member_edge:?}");
        assert_eq!(member_edge[0].rule, RuleId::Chk004);
        assert_eq!(member_edge[0].confidence, Confidence::Certain);

        let root_edge = run(import("pydocket", Some("slim"), false));
        assert_eq!(root_edge.len(), 1, "{root_edge:?}");
        assert_eq!(root_edge[0].rule, RuleId::Chk004);
        assert_eq!(root_edge[0].confidence, Confidence::Likely);

        let optional = run(import("pydocket", Some("slim"), true));
        assert_eq!(optional.len(), 1, "{optional:?}");
        assert_eq!(optional[0].rule, RuleId::Chk003);
        assert_eq!(optional[0].severity, Severity::Info);

        let root_code = run(import("pydocket", None, false));
        assert_eq!(root_code.len(), 1, "{root_code:?}");
        assert_eq!(root_code[0].rule, RuleId::Chk004);
        assert_eq!(root_code[0].confidence, Confidence::Certain);
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
            fallback_for: Vec::new(),
            version_minors: None,
        };
        let dynamic = |line, optional, platform_guarded| crate::parser::DynamicImport {
            module: "pkg".to_owned(),
            line,
            optional,
            platform_guarded,
            ..crate::parser::DynamicImport::default()
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

    /// One input row of `docs/dev/formal/deps_rules_z3.py`.
    #[derive(Debug, Clone, Copy)]
    struct FormalCase {
        usage: crate::parser::ImportContext,
        /// `[runtime, dev, type, optional]` buckets of `D`.
        root: [bool; 4],
        member: Option<[bool; 4]>,
        strict: bool,
        optional: bool,
        /// `(listed, edge from the root's declarations, edge from the member's)`.
        root_lock: Option<(bool, bool, bool)>,
        /// `(listed, edge from the member's declarations)`.
        member_lock: Option<(bool, bool)>,
    }

    /// `decide` in `deps_rules_z3.py`: rule, severity and confidence of each
    /// finding (CHK005's verdict is `misplaced.rs` `mod verification`'s).
    fn formal_decision(case: FormalCase) -> Vec<(RuleId, Option<(Severity, Confidence)>)> {
        use crate::parser::ImportContext;
        let matches = |[runtime, dev, ty, optional]: [bool; 4]| match case.usage {
            ImportContext::Runtime => runtime || optional,
            ImportContext::Type => ty || runtime || optional,
            ImportContext::Test | ImportContext::Dev => dev || runtime || optional,
        };
        let usage_runtime = case.usage == ImportContext::Runtime;
        let member = case.member.filter(|buckets| buckets.contains(&true));
        if member.is_some_and(matches) {
            return Vec::new();
        }
        let root = case.root.contains(&true).then_some(case.root);
        let governing = member.filter(|_| case.strict).or(root);
        let Some([runtime, dev, ty, optional]) = governing else {
            if !case.strict && !usage_runtime {
                return Vec::new();
            }
            let in_member = case.member.is_some();
            let root_edge = |(_, from_root, from_member): (bool, bool, bool)| {
                if in_member { from_member } else { from_root }
            };
            let edge = case.member_lock.is_some_and(|(_, edge)| edge)
                || case.root_lock.is_some_and(root_edge);
            let listed = case.member_lock.is_some_and(|(listed, _)| listed)
                || case.root_lock.is_some_and(|(listed, ..)| listed);
            let finding = if case.optional && !edge {
                let severity = if case.strict {
                    Severity::Warning
                } else {
                    Severity::Info
                };
                (RuleId::Chk003, severity, Confidence::Likely)
            } else if edge || listed {
                let severity = if case.optional {
                    Severity::Warning
                } else {
                    Severity::Error
                };
                let confidence = if edge {
                    Confidence::Certain
                } else {
                    Confidence::Likely
                };
                (RuleId::Chk004, severity, confidence)
            } else {
                (RuleId::Chk003, Severity::Error, Confidence::Certain)
            };
            return vec![(finding.0, Some((finding.1, finding.2)))];
        };
        if case.strict && case.member.is_some() && member.is_none() && matches(case.root) {
            return vec![(RuleId::Chk003, Some((Severity::Error, Confidence::Certain)))];
        }
        if usage_runtime && !(runtime || optional) && (dev || ty) {
            return vec![(RuleId::Chk005, None)];
        }
        Vec::new()
    }

    const FORMAL_D: &str = "pkg";

    /// Every input row of `deps_rules_z3.py` for the usage contexts that
    /// reach distinct rows (`Dev` behaves as `Test`).
    fn formal_cases() -> Vec<FormalCase> {
        use crate::parser::ImportContext;
        let bit = |bits: u8, k: u8| bits & (1 << k) != 0;
        let buckets = |bits: u8| [0, 1, 2, 3].map(|k| bit(bits, k));
        let root_locks = std::iter::once(None)
            .chain((0..8_u8).map(|bits| Some((bit(bits, 0), bit(bits, 1), bit(bits, 2)))));
        let member_locks: Vec<_> = std::iter::once(None)
            .chain((0..4_u8).map(|bits| Some((bit(bits, 0), bit(bits, 1)))))
            .collect();
        let members: Vec<_> = std::iter::once(None)
            .chain((0..16_u8).map(|bits| Some(buckets(bits))))
            .collect();
        let usages = [
            ImportContext::Runtime,
            ImportContext::Type,
            ImportContext::Test,
        ];
        let members = &members;
        let declarations = usages.into_iter().flat_map(|usage| {
            (0..16_u8).flat_map(move |root| {
                members
                    .iter()
                    .map(move |&member| (usage, buckets(root), member))
            })
        });
        let locks = |member: Option<[bool; 4]>| {
            let member_locks = member_locks[..if member.is_some() { 5 } else { 1 }].to_vec();
            root_locks.clone().flat_map(move |root_lock| {
                member_locks
                    .clone()
                    .into_iter()
                    .map(move |member_lock| (root_lock, member_lock))
            })
        };
        declarations
            .flat_map(|(usage, root, member)| {
                locks(member).flat_map(move |(root_lock, member_lock)| {
                    (0..4_u8).map(move |bits| FormalCase {
                        usage,
                        root,
                        member,
                        strict: bit(bits, 0),
                        optional: bit(bits, 1),
                        root_lock,
                        member_lock,
                    })
                })
            })
            .collect()
    }

    /// `pkg`'s entries in `buckets`, next to `other`, a runtime dependency
    /// whose lock edge may reach `pkg`.
    fn formal_index<'a>(
        entries: &'a [DeclaredDependency],
        buckets: [bool; 4],
        other: &'a DeclaredDependency,
    ) -> DeclaredIndex<'a> {
        let own: Vec<_> = entries
            .iter()
            .zip(buckets)
            .filter_map(|(entry, on)| on.then_some(entry))
            .collect();
        let mut index = BTreeMap::from([(other.name.clone(), vec![other])]);
        if !own.is_empty() {
            index.insert(FORMAL_D.to_owned(), own);
        }
        index
    }

    /// A lockfile listing `pkg` when `listed`, with an edge to it from each of `from`.
    fn formal_lock(listed: bool, from: &[(bool, &str)]) -> LockfileGraph {
        LockfileGraph {
            edges: from
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, name)| ((*name).to_owned(), vec![FORMAL_D.to_owned()]))
                .chain(listed.then(|| (FORMAL_D.to_owned(), Vec::new())))
                .collect(),
            ..LockfileGraph::default()
        }
    }

    /// `detect_missing_dependencies` and `detect_misplaced_dependencies` on
    /// one row built as real declarations and lockfiles.
    fn run_formal_case(case: FormalCase) -> Vec<(RuleId, Option<(Severity, Confidence)>)> {
        let entries = [
            DependencyContext::Runtime,
            DependencyContext::Group("dev".to_owned()),
            DependencyContext::Group("typing".to_owned()),
            DependencyContext::OptionalExtra("extra".to_owned()),
        ]
        .map(|context| declared_dep_in(FORMAL_D, context));
        let (root_dep, member_dep) = (declared_dep("rootdep"), declared_dep("memberdep"));
        let root_index = formal_index(&entries, case.root, &root_dep);
        let transitive = case.root_lock.map(|(listed, from_root, from_member)| {
            formal_lock(
                listed,
                &[(from_root, "rootdep"), (from_member, "memberdep")],
            )
        });
        let member_lock = case
            .member_lock
            .map(|(listed, edge)| formal_lock(listed, &[(edge, "memberdep")]));
        let workspace: Vec<_> = case
            .member
            .map(|buckets| WorkspaceDeclaredIndex {
                member_id: "api",
                declared: formal_index(&entries, buckets, &member_dep),
                lockfile: member_lock.as_ref(),
            })
            .into_iter()
            .collect();
        let import = ResolvedImport {
            context: case.usage,
            optional: case.optional,
            workspace_member: case.member.map(|_| "api".to_owned()),
            ..runtime_import(FORMAL_D)
        };
        let resolution = ResolutionIndex {
            imports: vec![Arc::new(import)],
            string_refs: Vec::new(),
            warnings: Vec::new(),
            transitive: transitive.clone().unwrap_or_default(),
            binary_resolutions: BTreeMap::new(),
            pytest_plugin_distributions: std::collections::BTreeSet::new(),
        };
        let config = default_config();
        let sources = sources();
        let parse = parse_for(&resolution.imports);
        let rules = RuleContext {
            resolution: &resolution,
            sources: &sources,
            graph: &crate::graph::ProjectGraph::new(sources.root.clone()),
            reachability: &crate::reachability::ReachabilityReport::default(),
            parse: &parse,
            surface: None,
        };
        let dependency = DependencyRuleContext {
            rules: &rules,
            config: &config,
            strict: case.strict,
            build_scripts: &BTreeMap::new(),
            local: &crate::sources::LocalTrees::default(),
        };
        let reachable = HashSet::from([FILE]);
        let mut found = detect_missing_dependencies(
            &root_index,
            &dependency,
            &reachable,
            transitive.is_some(),
            &workspace,
        );
        found.extend(super::super::misplaced::detect_misplaced_dependencies(
            &root_index,
            &dependency,
            &reachable,
            &workspace,
            &HashSet::new(),
        ));
        found.iter().map(formal_verdict).collect()
    }

    /// CHK005's severity and confidence are `misplaced.rs` `mod verification`'s.
    fn formal_verdict(candidate: &IssueCandidate) -> (RuleId, Option<(Severity, Confidence)>) {
        let verdict = (candidate.rule != RuleId::Chk005)
            .then_some((candidate.severity, candidate.confidence));
        (candidate.rule, verdict)
    }

    /// The decision table `docs/dev/formal/deps_rules_z3.py` checks is the one
    /// `detect_missing_dependencies` and `detect_misplaced_dependencies`
    /// implement (#1108): every usage context × root and member buckets ×
    /// `--strict` × optional import × what the root and member lockfiles say.
    #[test]
    fn decisions_follow_the_formal_model() {
        let cases = formal_cases();
        assert_eq!(cases.len(), 3 * 16 * (9 * 4 + 16 * 9 * 5 * 4));
        for case in cases {
            assert_eq!(run_formal_case(case), formal_decision(case), "{case:?}");
        }
    }
}
