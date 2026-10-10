//! Dependency reconciliation orchestration (pipeline step 10).

use std::collections::HashSet;

use indexmap::IndexSet;

use crate::config::Confidence;
use crate::manifest::{
    DeclaredDependency, DependencyContext, InlineScript, LoadedManifest,
    normalize_distribution_name,
};
use crate::plugins::PluginHints;
use crate::resolver::ImportMap;
use crate::rules::types::{
    DependencyReport, IssueCandidate, RuleId, Severity, WorkspaceDependencyBoundary,
    sort_candidates,
};
use crate::rules::{DependencyRuleContext, RuleContext};
use crate::sources::{DiscoveredSources, PublicSurface};

use super::binary::{BinaryProviders, detect_unlisted_binaries};
use super::duplicate::detect_duplicate_dependencies;
use super::misplaced::{detect_misplaced_dependencies, pytest11_modules};
use super::missing::{WorkspaceDeclaredIndex, detect_missing_dependencies};
use super::script::{detect_script_dependency_issues, is_script_third_party};
use super::unused::{UnusedEvidenceContext, detect_unused_dependencies};
use super::used::{
    DeclaredIndex, build_declared_index, build_import_declared_index, collect_used_distributions,
    has_lockfile, mark_pytest_plugin_distributions, mark_self_referential_distribution,
    mark_workspace_source_distributions, reachable_paths, usage_paths,
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

/// An unreadable `install_requires` may declare any import, so CHK003/CHK004
/// become hints rather than errors (#491).
fn lower_missing_when_runtime_dependencies_unknown(missing: &mut [IssueCandidate]) {
    for candidate in missing {
        if matches!(candidate.rule, RuleId::Chk003 | RuleId::Chk004) {
            candidate.severity = Severity::Info;
            candidate.confidence = Confidence::Maybe;
            candidate
                .explain
                .details
                .push("runtime dependencies could not be read statically".to_owned());
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
    let workspace_declared = workspace_declared_indices(manifest, workspace_boundaries);
    let import_declared = build_import_declared_index(manifest, &manifest.lockfile);
    let lockfile_present = has_lockfile(manifest, resolution);
    let reachable = reachable_paths(graph, reachability);
    let usage = usage_paths(&reachable, reachability);

    let mut used = collect_used_distributions(context, plugins, &usage);

    mark_self_referential_distribution(manifest, &declared, &mut used);
    mark_workspace_source_distributions(manifest, context, &usage, workspace_boundaries, &mut used);

    for distribution in plugins.config_used_distributions() {
        used.insert(distribution.clone());
    }
    mark_pytest_plugin_distributions(resolution, &mut used);

    mark_companion_distributions(&declared, &mut used);

    let mut candidates = Vec::new();
    let evidence = UnusedEvidenceContext {
        rules: context,
        reachable: &reachable,
        build_requires: &manifest.metadata.build_requires,
    };

    let metapackage = ships_no_code(manifest, context.sources);
    for deps in declared.values() {
        let deps: Vec<&DeclaredDependency> = deps
            .iter()
            .copied()
            .filter(|dep| !metapackage || matches!(dep.context, DependencyContext::Group(_)))
            .collect();
        candidates.extend(detect_unused_dependencies(
            &deps,
            &used,
            config,
            strict,
            Some(&evidence),
        ));
    }
    lower_unused_when_a_skipped_file_is_reached(&mut candidates, context, &reachable);

    let mut missing = detect_missing_dependencies(
        &import_declared,
        dependency,
        &reachable,
        lockfile_present,
        &workspace_declared,
    );
    if manifest.sources.runtime_dependencies_unknown {
        lower_missing_when_runtime_dependencies_unknown(&mut missing);
    }
    candidates.extend(missing);

    candidates.extend(detect_misplaced_dependencies(
        &import_declared,
        dependency,
        &reachable,
        &workspace_declared,
        &pytest11_modules(
            std::iter::once(manifest).chain(workspace_boundaries.iter().map(|b| b.manifest)),
        ),
    ));

    let providers = BinaryProviders::new(&declared, manifest, workspace_boundaries, plugins);
    candidates.extend(detect_unlisted_binaries(&providers, resolution, plugins));

    candidates.extend(detect_duplicate_dependencies(
        &manifest.dependencies,
        manifest.metadata.name.as_deref(),
    ));

    sort_candidates(&mut candidates);

    DependencyReport {
        candidates,
        used_distributions: used,
    }
}

/// A wheel target that matches no source file ships a metapackage, whose
/// distributed dependencies are its content rather than imports (#529).
/// Dependency groups are not distributed, so they are still checked.
fn ships_no_code(manifest: &LoadedManifest, sources: &DiscoveredSources) -> bool {
    manifest
        .metadata
        .wheel_targets
        .as_ref()
        .is_some_and(|targets| PublicSurface::resolve(Some(targets), &sources.files).is_none())
}

fn workspace_declared_indices<'a>(
    root: &LoadedManifest,
    workspace_boundaries: &[WorkspaceDependencyBoundary<'a>],
) -> Vec<WorkspaceDeclaredIndex<'a>> {
    workspace_boundaries
        .iter()
        .map(|boundary| {
            // A uv workspace member reads the root's lockfile; a member with
            // its own (langchain's `libs/core/uv.lock`) reads that (#653).
            let own_lockfile = boundary
                .manifest
                .sources
                .lockfile
                .is_some()
                .then_some(&boundary.manifest.lockfile);
            WorkspaceDeclaredIndex {
                member_id: boundary.member_id,
                declared: build_import_declared_index(
                    boundary.manifest,
                    own_lockfile.unwrap_or(&root.lockfile),
                ),
                lockfile: own_lockfile,
            }
        })
        .collect()
}

/// Distributions another library imports lazily at runtime, so the project
/// declares them without importing them: starlette's `request.form()` imports
/// `python_multipart` only when called.
const RUNTIME_PEERS: &[(&str, &[&str])] = &[("python-multipart", &["starlette", "fastapi"])];

/// Mark `types-*` stubs and [`RUNTIME_PEERS`] used when the package they serve is.
fn mark_companion_distributions(declared: &DeclaredIndex<'_>, used: &mut IndexSet<String>) {
    for name in declared.keys() {
        if let Some(runtime) = runtime_for_stub(name)
            && used.contains(&normalize_distribution_name(runtime))
        {
            used.insert(name.clone());
        }
    }
    for (peer, providers) in RUNTIME_PEERS {
        if declared.contains_key(*peer) && providers.iter().any(|provider| used.contains(*provider))
        {
            used.insert((*peer).to_owned());
        }
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
                    package_root: "src".to_owned(),
                    ..Default::default()
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
            config_declared_distributions: Vec::new(),
            config_provided_binaries: Vec::new(),
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
            config_declared_distributions: Vec::new(),
            config_provided_binaries: Vec::new(),
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
    fn metapackage_keeps_distributed_dependencies_out_of_chk002() {
        let dep = |name: &str, context: DependencyContext| DeclaredDependency {
            name: name.to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(5),
                label: format!("{name} declaration"),
            },
            opaque: false,
            included_via: Vec::new(),
        };
        let mut manifest = minimal_manifest(vec![
            dep("acme-core", DependencyContext::Runtime),
            dep(
                "acme-extra",
                DependencyContext::OptionalExtra("all".to_owned()),
            ),
            dep("pytest-mock", DependencyContext::Group("dev".to_owned())),
        ]);
        manifest.metadata.wheel_targets = Some(crate::manifest::WheelTargets {
            source: "tool.hatch.build.targets.wheel".to_owned(),
            paths: vec!["_meta/acme".to_owned()],
            find: Vec::new(),
        });
        let resolution = ResolutionIndex::default();
        let reachability = ReachabilityReport::default();
        let plugins = PluginHints {
            contributions: Vec::new(),
            config_binary_usages: Vec::new(),
            config_used_distributions: Vec::new(),
            config_declared_distributions: Vec::new(),
            config_provided_binaries: Vec::new(),
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
                strict: true,
            },
            &manifest,
            &plugins,
            &[],
            &[],
        );
        let reported: Vec<_> = report
            .candidates
            .iter()
            .filter(|candidate| candidate.rule == crate::rules::types::RuleId::Chk002)
            .map(|candidate| candidate.explain.summary.as_str())
            .collect();
        assert_eq!(reported, ["pytest-mock is declared but not used"]);
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
            config_declared_distributions: Vec::new(),
            config_provided_binaries: Vec::new(),
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
