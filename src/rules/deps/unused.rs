//! CHK002 unused dependency detection.

use std::collections::{BTreeSet, HashSet};

use crate::config::{ChokkinConfig, Confidence};
use crate::graph::{GraphEdge, ProjectGraph};
use crate::manifest::DeclaredDependency;
use crate::manifest::normalize_distribution_name;
use crate::reachability::{ReachabilityReport, UnreachableReason};
use crate::resolver::{ResolutionIndex, import_root};
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};

use super::context::{DeclarationBucket, declaration_bucket};

/// Context for building CHK002 reachability evidence in `--explain` output.
pub(super) struct UnusedEvidenceContext<'a> {
    pub resolution: &'a ResolutionIndex,
    pub reachability: &'a ReachabilityReport,
    pub graph: &'a ProjectGraph,
    pub reachable: &'a HashSet<String>,
}

/// Detect declared dependencies with no matching usage.
pub(super) fn detect_unused_dependencies(
    declared: &[&DeclaredDependency],
    used: &indexmap::IndexSet<String>,
    config: &ChokkinConfig,
    strict: bool,
    evidence: Option<&UnusedEvidenceContext<'_>>,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for dep in declared {
        if dep.opaque {
            continue;
        }
        if !seen.insert(dep.name.as_str()) {
            continue;
        }
        if used.contains(&dep.name) {
            continue;
        }
        if should_suppress_unused_report(&dep.context, config, strict) {
            continue;
        }
        if !strict && dep.marker.is_some() {
            continue;
        }

        let (confidence, severity) = unused_confidence(dep, strict);
        let mut details = vec![
            format!("declaration: {}", dep.origin.label),
            "no import, plugin module ref, or binary usage resolved to this distribution"
                .to_owned(),
        ];
        if let Some(context) = evidence {
            details.extend(build_reachability_evidence(&dep.name, context));
        }

        candidates.push(IssueCandidate {
            rule: RuleId::Chk002,
            subject: IssueSubject::Distribution {
                name: dep.name.clone(),
            },
            severity,
            confidence,
            message: format!(
                "declared in {}, no reachable import, config, or binary usage found",
                dep.origin.label
            ),
            workspace_member: None,
            origins: vec![Origin::Manifest(dep.origin.clone())],
            explain: ExplainData {
                summary: format!("{} is declared but not used", dep.name),
                details,
            },
        });
    }

    candidates
}

fn build_reachability_evidence(
    distribution: &str,
    context: &UnusedEvidenceContext<'_>,
) -> Vec<String> {
    let top_level = top_level_modules_for_distribution(distribution, context);
    let mut details = Vec::new();
    if top_level.is_empty() {
        return details;
    }

    let module_list = top_level.into_iter().collect::<Vec<_>>().join(", ");
    details.push(format!("top-level modules: {module_list}"));

    let distribution_imports = context
        .resolution
        .imports
        .iter()
        .filter(|import| {
            import
                .distribution
                .as_deref()
                .is_some_and(|name| name == distribution)
        })
        .collect::<Vec<_>>();

    let reachable_imports = distribution_imports
        .iter()
        .filter(|import| context.reachable.contains(&import.file))
        .collect::<Vec<_>>();
    let unreachable_imports = distribution_imports
        .iter()
        .filter(|import| !context.reachable.contains(&import.file))
        .collect::<Vec<_>>();

    if reachable_imports.is_empty() {
        details.push("evidence (unreachable):".to_owned());
        details.push(format!("  - no reachable file imports {module_list}"));
        for import in unreachable_imports {
            let suffix = unreachable_file_suffix(&import.file, context.reachability);
            details.push(format!(
                "  - {}:{} imports {} (file is unreachable{suffix})",
                import.file, import.line, import.full_module
            ));
        }
    } else {
        details.push("evidence (reachable imports found, but not counted as used):".to_owned());
        for import in reachable_imports {
            details.push(format!(
                "  - {}:{} imports {}",
                import.file, import.line, import.full_module
            ));
        }
    }

    details
}

fn top_level_modules_for_distribution(
    distribution: &str,
    context: &UnusedEvidenceContext<'_>,
) -> BTreeSet<String> {
    let mut modules = BTreeSet::new();

    if let Some(distribution_id) = context.graph.distribution_id(distribution) {
        for edge in context.graph.edges() {
            if let GraphEdge::DistributionProvidesModule {
                distribution,
                module,
            } = edge
                && *distribution == distribution_id
                && let Some(module_node) = context.graph.module(*module)
            {
                modules.insert(import_root(&module_node.name).to_owned());
            }
        }
    }

    for import in &context.resolution.imports {
        if import
            .distribution
            .as_deref()
            .is_some_and(|name| name == distribution)
        {
            modules.insert(import.import_root.clone());
        }
    }

    if modules.is_empty() {
        modules.insert(normalize_distribution_name(distribution));
    }

    modules
}

fn unreachable_file_suffix(path: &str, reachability: &ReachabilityReport) -> String {
    let Some(file) = reachability
        .unreachable
        .iter()
        .find(|candidate| candidate.path == path)
    else {
        return String::new();
    };

    if file
        .reasons
        .iter()
        .any(|reason| matches!(reason, UnreachableReason::NotReachable))
    {
        return ", CHK001".to_owned();
    }

    let label = file
        .reasons
        .iter()
        .find_map(|reason| match reason {
            UnreachableReason::ExcludedProductionContext => Some("excluded in production"),
            UnreachableReason::ExcludedTestContext => Some("excluded test context"),
            UnreachableReason::ExcludedInit => Some("excluded __init__.py"),
            UnreachableReason::ExcludedStub => Some("excluded stub"),
            UnreachableReason::FrameworkUsed => Some("framework-used"),
            UnreachableReason::NotReachable => None,
        })
        .unwrap_or("unreachable");

    format!(", {label}")
}

/// Dev, optional-extra, and setup-extra declarations are not reported unless `--strict`.
fn should_suppress_unused_report(
    context: &crate::manifest::DependencyContext,
    config: &ChokkinConfig,
    strict: bool,
) -> bool {
    if strict {
        return false;
    }
    if declaration_bucket(context, &config.dependencies) == DeclarationBucket::Dev {
        return true;
    }
    matches!(
        context,
        crate::manifest::DependencyContext::OptionalExtra(_)
            | crate::manifest::DependencyContext::SetupExtra(_)
    )
}

fn unused_confidence(dep: &DeclaredDependency, strict: bool) -> (Confidence, Severity) {
    if dep.marker.is_some() {
        let severity = if strict {
            Severity::Error
        } else {
            Severity::Warning
        };
        return (Confidence::Likely, severity);
    }
    (Confidence::Certain, Severity::Error)
}

/// Whether a `types-*` stub name looks like a types stub package.
#[must_use]
pub(super) fn is_types_stub(name: &str) -> bool {
    let normalized = normalize_distribution_name(name);
    normalized.starts_with("types-") || normalized.ends_with("-stubs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::{FileNode, ModuleOrigin, ProjectGraph};
    use crate::manifest::{DeclaredDependency, DependencyContext, DependencyOrigin};
    use crate::parser::ImportContext;
    use crate::reachability::ReachabilityReport;
    use crate::resolver::{ResolutionIndex, ResolveConfidence, ResolvedImport};
    use crate::sources::{FileContext, FileKind};

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
                label: "dependency-groups.dev[0]".to_owned(),
            },
            opaque: false,
        }
    }

    fn boto3_dep() -> DeclaredDependency {
        DeclaredDependency {
            name: "boto3".to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context: DependencyContext::Runtime,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(18),
                label: "project.dependencies[0]".to_owned(),
            },
            opaque: false,
        }
    }

    #[test]
    fn suppresses_unused_dev_group_by_default() {
        let config = default_config();
        let dev_dep = dep(DependencyContext::Group("dev".to_owned()));
        let candidates = detect_unused_dependencies(
            &[&dev_dep],
            &indexmap::IndexSet::new(),
            &config,
            false,
            None,
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn reports_unused_dev_group_in_strict_mode() {
        let config = default_config();
        let dev_dep = dep(DependencyContext::Group("dev".to_owned()));
        let candidates = detect_unused_dependencies(
            &[&dev_dep],
            &indexmap::IndexSet::new(),
            &config,
            true,
            None,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].rule, RuleId::Chk002);
    }

    #[test]
    fn reports_unused_runtime_dependency() {
        let config = default_config();
        let runtime_dep = dep(DependencyContext::Runtime);
        let candidates = detect_unused_dependencies(
            &[&runtime_dep],
            &indexmap::IndexSet::new(),
            &config,
            false,
            None,
        );
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn suppresses_unused_optional_extra_by_default() {
        let config = default_config();
        let optional_dep = dep(DependencyContext::OptionalExtra("brotli".to_owned()));
        let candidates = detect_unused_dependencies(
            &[&optional_dep],
            &indexmap::IndexSet::new(),
            &config,
            false,
            None,
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn explain_includes_unreachable_import_evidence() {
        let config = default_config();
        let dep = boto3_dep();
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        });
        let reachable_file = graph
            .intern_file(FileNode {
                path: "src/acme/main.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("reachable file");
        let legacy_file = graph
            .intern_file(FileNode {
                path: "src/legacy/aws.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("legacy file");
        let _ = graph.intern_module(
            "boto3".to_owned(),
            ModuleOrigin::ThirdParty,
        );
        let _ = graph.ensure_distribution("boto3");
        let _ = graph.intern_module("botocore".to_owned(), ModuleOrigin::ThirdParty);

        let mut reachability = ReachabilityReport::empty();
        reachability.reachable.insert(reachable_file);
        reachability.unreachable.push(crate::reachability::UnreachableFile {
            file: legacy_file,
            path: "src/legacy/aws.py".to_owned(),
            reasons: vec![UnreachableReason::NotReachable],
            max_confidence: Confidence::Certain,
        });

        let mut resolution = ResolutionIndex::empty();
        resolution.imports.push(ResolvedImport {
            import_root: "boto3".to_owned(),
            full_module: "boto3".to_owned(),
            file: "src/legacy/aws.py".to_owned(),
            workspace_member: None,
            line: 5,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            origin: ModuleOrigin::ThirdParty,
            distribution: Some("boto3".to_owned()),
            confidence: ResolveConfidence::Certain,
        });

        let reachable = HashSet::from(["src/acme/main.py".to_owned()]);
        let evidence = UnusedEvidenceContext {
            resolution: &resolution,
            reachability: &reachability,
            graph: &graph,
            reachable: &reachable,
        };

        let candidates = detect_unused_dependencies(
            &[&dep],
            &indexmap::IndexSet::new(),
            &config,
            false,
            Some(&evidence),
        );
        let explain = &candidates[0].explain;
        assert!(explain.details.iter().any(|line| line.contains("top-level modules:")));
        assert!(explain
            .details
            .iter()
            .any(|line| line.contains("no reachable file imports")));
        assert!(explain.details.iter().any(|line| {
            line.contains("src/legacy/aws.py:5 imports boto3")
                && line.contains("CHK001")
        }));
    }
}
