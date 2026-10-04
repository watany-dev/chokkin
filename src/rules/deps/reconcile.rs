//! Dependency reconciliation orchestration (pipeline step 10).

use std::collections::HashSet;

use crate::config::Confidence;
use crate::manifest::{InlineScript, LoadedManifest, normalize_distribution_name};
use crate::plugins::PluginHints;
use crate::resolver::ImportMap;
use crate::rules::types::{
    DependencyReport, IssueCandidate, WorkspaceDependencyBoundary, sort_candidates,
};
use crate::rules::{DependencyRuleContext, RuleContext};

use super::binary::detect_unlisted_binaries;
use super::duplicate::detect_duplicate_dependencies;
use super::misplaced::detect_misplaced_dependencies;
use super::missing::detect_missing_dependencies;
use super::script::{detect_script_dependency_issues, is_script_third_party};
use super::unused::{UnusedEvidenceContext, detect_unused_dependencies};
use super::used::{
    build_declared_index, collect_used_distributions, has_lockfile,
    mark_pytest_plugin_distributions, mark_self_referential_distribution,
    mark_workspace_source_distributions, reachable_paths,
};

/// Reconcile declared dependencies against imports, plugins, and binaries (§10).
///
/// The project manifest and each PEP 723 script block are reconciled separately.
#[must_use]
pub fn reconcile_with_context(
    dependency: &DependencyRuleContext<'_>,
    manifest: &LoadedManifest,
    plugins: &PluginHints,
    workspace_boundaries: &[WorkspaceDependencyBoundary<'_>],
    scripts: &[InlineScript],
) -> DependencyReport {
    if scripts.is_empty() {
        return reconcile_project(dependency, manifest, plugins, workspace_boundaries);
    }
    let script_paths: HashSet<&str> = scripts.iter().map(|script| script.path.as_str()).collect();
    let mut project_resolution = dependency.rules.resolution.clone();
    project_resolution
        .imports
        .retain(|import| !is_script_third_party(import, &script_paths));
    let project_rules = RuleContext {
        resolution: &project_resolution,
        ..*dependency.rules
    };
    let project = DependencyRuleContext {
        rules: &project_rules,
        ..*dependency
    };
    let mut report = reconcile_project(&project, manifest, plugins, workspace_boundaries);
    let reachable = reachable_paths(dependency.rules.graph, dependency.rules.reachability);
    report.candidates.extend(detect_script_dependency_issues(
        scripts,
        dependency.rules.resolution,
        &reachable,
        &dependency.rules.sources.files,
        dependency.rules.parse,
        &ImportMap::build(dependency.config),
        dependency.strict,
    ));
    sort_candidates(&mut report.candidates);
    report
}

/// A reachable file that could not be decoded may import any declared
/// dependency, so none is certainly unused (and `--fix` must not remove it).
fn lower_unused_when_a_skipped_file_is_reached(
    unused: &mut [IssueCandidate],
    context: &RuleContext<'_>,
    reachable: &HashSet<&str>,
) {
    if context
        .parse
        .modules
        .iter()
        .any(|module| module.skipped && reachable.contains(module.path.as_str()))
    {
        for candidate in unused {
            candidate.confidence = candidate.confidence.min(Confidence::Likely);
        }
    }
}

fn reconcile_project(
    dependency: &DependencyRuleContext<'_>,
    manifest: &LoadedManifest,
    plugins: &PluginHints,
    workspace_boundaries: &[WorkspaceDependencyBoundary<'_>],
) -> DependencyReport {
    let DependencyRuleContext {
        rules: context,
        config,
        strict,
    } = *dependency;
    let RuleContext {
        resolution,
        reachability,
        graph,
        ..
    } = *context;
    let declared = build_declared_index(manifest);
    let workspace_declared = workspace_boundaries
        .iter()
        .map(|boundary| super::missing::WorkspaceDeclaredIndex {
            member_id: boundary.member_id,
            declared: build_declared_index(boundary.manifest),
        })
        .collect::<Vec<_>>();
    let lockfile_present = has_lockfile(manifest, resolution);
    let reachable = reachable_paths(graph, reachability);

    let mut used = collect_used_distributions(context, plugins, &reachable);

    mark_self_referential_distribution(manifest, &declared, &mut used);
    mark_workspace_source_distributions(
        manifest,
        context,
        &reachable,
        workspace_boundaries,
        &mut used,
    );

    for distribution in plugins.config_used_distributions() {
        used.insert(distribution.clone());
    }
    mark_pytest_plugin_distributions(resolution, &mut used);

    // types-* stubs are considered used when their runtime package is used.
    for name in declared.keys() {
        if let Some(runtime) = runtime_for_stub(name)
            && used.contains(&normalize_distribution_name(runtime))
        {
            used.insert(name.clone());
        }
    }

    let mut candidates = Vec::new();
    let evidence = UnusedEvidenceContext {
        rules: context,
        reachable: &reachable,
        build_requires: &manifest.metadata.build_requires,
    };

    for deps in declared.values() {
        candidates.extend(detect_unused_dependencies(
            deps,
            &used,
            config,
            strict,
            Some(&evidence),
        ));
    }
    lower_unused_when_a_skipped_file_is_reached(&mut candidates, context, &reachable);

    candidates.extend(detect_missing_dependencies(
        &declared,
        dependency,
        &reachable,
        lockfile_present,
        &workspace_declared,
    ));

    candidates.extend(detect_misplaced_dependencies(
        &declared,
        dependency,
        &reachable,
        &workspace_declared,
    ));

    candidates.extend(detect_unlisted_binaries(&declared, resolution, plugins));

    candidates.extend(detect_duplicate_dependencies(
        &manifest.dependencies,
        config,
    ));

    sort_candidates(&mut candidates);

    DependencyReport {
        candidates,
        used_distributions: used,
    }
}

/// Map a `types-*` stub name to its runtime package when the pattern is known.
fn runtime_for_stub(stub_name: &str) -> Option<&str> {
    stub_name
        .strip_prefix("types-")
        .or_else(|| stub_name.strip_suffix("-stubs"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::ProjectGraph;
    use crate::manifest::{
        DeclaredDependency, DependencyContext, DependencyOrigin, LoadedManifest, LockfileGraph,
        ManifestSources, ProjectMetadata,
    };
    use crate::parser::ParseSummary;
    use crate::plugins::PluginHints;
    use crate::reachability::ReachabilityReport;
    use crate::resolver::ResolutionIndex;
    use crate::sources::DiscoveredSources;

    fn minimal_manifest(deps: Vec<DeclaredDependency>) -> LoadedManifest {
        LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            metadata: ProjectMetadata::default(),
            dependencies: deps,
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings::default(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    fn reconcile_inputs(
        manifest: &LoadedManifest,
    ) -> (DiscoveredSources, ParseSummary, ProjectGraph) {
        (
            DiscoveredSources {
                root: manifest.root.clone(),
                layout: crate::sources::LayoutInfo {
                    layout: crate::sources::ProjectLayout::Src,
                    packages: Vec::new(),
                    local_packages: Vec::new(),
                    inferred_globs: Vec::new(),
                },
                effective_globs: Vec::new(),
                files: Vec::new(),
                warnings: Vec::new(),
            },
            ParseSummary::default(),
            ProjectGraph::new(manifest.root.clone()),
        )
    }

    #[test]
    fn runtime_for_stub_maps_types_prefix() {
        assert_eq!(runtime_for_stub("types-requests"), Some("requests"));
    }

    #[test]
    fn declared_and_imported_dependency_produces_no_candidates() {
        let dep = DeclaredDependency {
            name: "requests".to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context: DependencyContext::Runtime,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(5),
                label: "project.dependencies[0]".to_owned(),
            },
            opaque: false,
            included_via: Vec::new(),
        };
        let manifest = minimal_manifest(vec![dep]);
        let (sources, parse, mut graph) = reconcile_inputs(&manifest);
        let file_id = graph
            .intern_file(crate::graph::FileNode {
                path: "src/app.py".to_owned(),
                context: crate::sources::FileContext::Runtime,
                kind: crate::sources::FileKind::Python,
            })
            .expect("file id");
        let mut reachability = ReachabilityReport::default();
        reachability.reachable.insert(file_id);
        let mut resolution = ResolutionIndex::default();
        resolution.imports.push(crate::resolver::ResolvedImport {
            import_root: "requests".to_owned(),
            full_module: "requests".to_owned(),
            file: "src/app.py".to_owned(),
            workspace_member: None,
            line: 1,
            context: crate::parser::ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            origin: crate::graph::ModuleOrigin::ThirdParty,
            distribution: Some("requests".to_owned()),
            confidence: crate::resolver::ResolveConfidence::Certain,
        });
        let plugins = PluginHints {
            contributions: Vec::new(),
            config_binary_usages: Vec::new(),
            config_used_distributions: Vec::new(),
            config_module_refs: Vec::new(),
            warnings: Vec::new(),
        };
        let config = crate::config::default_config();
        let report = reconcile_with_context(
            &DependencyRuleContext {
                rules: &RuleContext {
                    resolution: &resolution,
                    reachability: &reachability,
                    graph: &graph,
                    sources: &sources,
                    parse: &parse,
                },
                config: &config,
                strict: false,
            },
            &manifest,
            &plugins,
            &[],
            &[],
        );
        assert!(report.candidates.is_empty(), "{:?}", report.candidates);
        assert_eq!(
            report.used_distributions.iter().collect::<Vec<_>>(),
            ["requests"]
        );
    }

    #[test]
    fn unused_dependency_generates_chk002() {
        let dep = DeclaredDependency {
            name: "boto3".to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context: DependencyContext::Runtime,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(5),
                label: "project.dependencies[0]".to_owned(),
            },
            opaque: false,
            included_via: Vec::new(),
        };
        let manifest = minimal_manifest(vec![dep]);
        let resolution = ResolutionIndex::default();
        let reachability = ReachabilityReport::default();
        let plugins = PluginHints {
            contributions: Vec::new(),
            config_binary_usages: Vec::new(),
            config_used_distributions: Vec::new(),
            config_module_refs: Vec::new(),
            warnings: Vec::new(),
        };
        let config = crate::config::default_config();
        let (sources, parse, graph) = reconcile_inputs(&manifest);
        let report = reconcile_with_context(
            &DependencyRuleContext {
                rules: &RuleContext {
                    resolution: &resolution,
                    reachability: &reachability,
                    graph: &graph,
                    sources: &sources,
                    parse: &parse,
                },
                config: &config,
                strict: false,
            },
            &manifest,
            &plugins,
            &[],
            &[],
        );
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(
            report.candidates[0].rule,
            crate::rules::types::RuleId::Chk002
        );
    }

    #[test]
    fn types_stub_marked_used_when_runtime_is_used() {
        let dep = DeclaredDependency {
            name: "types-PyYAML".to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context: DependencyContext::Runtime,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(5),
                label: "project.dependencies[0]".to_owned(),
            },
            opaque: false,
            included_via: Vec::new(),
        };
        let manifest = minimal_manifest(vec![dep]);
        let (sources, parse, mut graph) = reconcile_inputs(&manifest);
        let file_id = graph
            .intern_file(crate::graph::FileNode {
                path: "src/app.py".to_owned(),
                context: crate::sources::FileContext::Runtime,
                kind: crate::sources::FileKind::Python,
            })
            .expect("file id");
        let mut reachability = ReachabilityReport::default();
        reachability.reachable.insert(file_id);
        let mut resolution = ResolutionIndex::default();
        resolution.imports.push(crate::resolver::ResolvedImport {
            import_root: "yaml".to_owned(),
            full_module: "yaml".to_owned(),
            file: "src/app.py".to_owned(),
            workspace_member: None,
            line: 1,
            context: crate::parser::ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            origin: crate::graph::ModuleOrigin::ThirdParty,
            distribution: Some("pyyaml".to_owned()),
            confidence: crate::resolver::ResolveConfidence::Certain,
        });
        let plugins = PluginHints {
            contributions: Vec::new(),
            config_binary_usages: Vec::new(),
            config_used_distributions: Vec::new(),
            config_module_refs: Vec::new(),
            warnings: Vec::new(),
        };
        let config = crate::config::default_config();
        let report = reconcile_with_context(
            &DependencyRuleContext {
                rules: &RuleContext {
                    resolution: &resolution,
                    reachability: &reachability,
                    graph: &graph,
                    sources: &sources,
                    parse: &parse,
                },
                config: &config,
                strict: false,
            },
            &manifest,
            &plugins,
            &[],
            &[],
        );
        assert!(report.used_distributions.contains("types-PyYAML"));
        assert!(
            !report
                .candidates
                .iter()
                .any(|candidate| candidate.rule == crate::rules::types::RuleId::Chk002)
        );
    }
}
