//! Build the set of used distributions from imports, plugins, and binaries.

use std::collections::{BTreeMap, HashSet};

use indexmap::IndexSet;

use crate::graph::{ModuleOrigin, ProjectGraph};
use crate::manifest::LoadedManifest;
use crate::plugins::PluginHints;
use crate::reachability::ReachabilityReport;
use crate::resolver::ResolutionIndex;
use crate::rules::RuleContext;

/// Index of declared dependencies keyed by normalized distribution name.
pub(super) type DeclaredIndex<'a> = BTreeMap<String, Vec<&'a crate::manifest::DeclaredDependency>>;

/// Build declared dependency index from manifest.
pub(super) fn build_declared_index(manifest: &LoadedManifest) -> DeclaredIndex<'_> {
    let mut index: DeclaredIndex<'_> = BTreeMap::new();
    for dep in &manifest.dependencies {
        index.entry(dep.name.clone()).or_default().push(dep);
    }
    index
}

/// Collect root-relative paths of reachable Python files.
pub(super) fn reachable_paths(
    graph: &ProjectGraph,
    reachability: &ReachabilityReport,
) -> HashSet<String> {
    reachability
        .reachable
        .iter()
        .filter_map(|file_id| graph.file(*file_id).map(|node| node.path.clone()))
        .collect()
}

/// Whether the project has lockfile data for transitive checks.
#[must_use]
pub(super) fn has_lockfile(manifest: &LoadedManifest, resolution: &ResolutionIndex) -> bool {
    manifest.sources.lockfile.is_some() || !resolution.transitive.edges.is_empty()
}

/// Distributions used by reachable imports, plugin refs, and binaries.
pub(super) fn collect_used_distributions(
    context: &RuleContext<'_>,
    plugins: &PluginHints,
) -> IndexSet<String> {
    let RuleContext {
        resolution,
        reachability,
        graph,
        ..
    } = *context;
    let binary_resolutions = &resolution.binary_resolutions;
    let reachable = reachable_paths(graph, reachability);
    let mut used = IndexSet::new();

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
        used.insert(distribution.clone());
    }

    for usage in plugins.all_binary_usages() {
        if let Some(distribution) = binary_resolutions.get(&usage.binary) {
            used.insert(distribution.clone());
        }
    }

    used
}

/// Treat a project's own distribution as used when declared (self-referential extras).
pub(super) fn mark_self_referential_distribution(
    manifest: &LoadedManifest,
    declared: &DeclaredIndex<'_>,
    used: &mut IndexSet<String>,
) {
    let Some(project_name) = manifest.metadata.name.as_ref() else {
        return;
    };
    let normalized = crate::manifest::normalize_distribution_name(project_name);
    if declared.contains_key(&normalized) {
        used.insert(normalized);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::{FileNode, ProjectGraph};
    use crate::manifest::{LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata};
    use crate::parser::ImportContext;
    use crate::reachability::ReachabilityReport;
    use crate::resolver::{ResolveConfidence, ResolvedImport, TransitiveIndex};

    #[test]
    fn detects_lockfile_from_manifest_sources() {
        let manifest = LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources {
                lockfile: Some(crate::manifest::LockfileSource {
                    kind: crate::manifest::LockfileKind::Uv,
                    path: "uv.lock".to_owned(),
                }),
                ..ManifestSources::default()
            },
            warnings: Vec::new(),
        };
        let resolution = ResolutionIndex::default();
        assert!(has_lockfile(&manifest, &resolution));
    }

    #[test]
    fn collects_third_party_from_reachable_import() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        });
        let file_id = graph
            .intern_file(FileNode {
                path: "src/app.py".to_owned(),
                context: crate::sources::FileContext::Runtime,
                kind: crate::sources::FileKind::Python,
            })
            .expect("file id");
        let reachable = {
            let mut report = ReachabilityReport::default();
            report.reachable.insert(file_id);
            report
        };
        let resolution = ResolutionIndex {
            imports: vec![ResolvedImport {
                import_root: "yaml".to_owned(),
                full_module: "yaml".to_owned(),
                file: "src/app.py".to_owned(),
                workspace_member: None,
                line: 1,
                context: ImportContext::Runtime,
                optional: false,
                platform_guarded: false,
                origin: ModuleOrigin::ThirdParty,
                distribution: Some("pyyaml".to_owned()),
                confidence: ResolveConfidence::Certain,
            }],
            warnings: Vec::new(),
            transitive: TransitiveIndex::default(),
            binary_resolutions: BTreeMap::new(),
        };
        let sources = crate::sources::DiscoveredSources {
            root: graph.root.clone(),
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Src,
                packages: Vec::new(),
                inferred_globs: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let used = collect_used_distributions(
            &RuleContext {
                resolution: &resolution,
                reachability: &reachable,
                graph: &graph,
                sources: &sources,
                parse: &crate::parser::ParseSummary::default(),
            },
            &PluginHints {
                contributions: Vec::new(),
                config_binary_usages: Vec::new(),
                config_used_distributions: Vec::new(),
                warnings: Vec::new(),
            },
        );
        assert!(used.contains("pyyaml"));
    }

    #[test]
    fn marks_self_referential_distribution_as_used() {
        let dep = crate::manifest::DeclaredDependency {
            name: "self-extra".to_owned(),
            extras: vec!["benchmark".to_owned()],
            marker: None,
            specifier: None,
            context: crate::manifest::DependencyContext::Runtime,
            origin: crate::manifest::DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(4),
                label: "project.dependencies[0]".to_owned(),
            },
            opaque: false,
        };
        let manifest = LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            metadata: ProjectMetadata {
                name: Some("self-extra".to_owned()),
                ..ProjectMetadata::default()
            },
            dependencies: vec![dep],
            constraints: Vec::new(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        };
        let declared = build_declared_index(&manifest);
        let mut used = IndexSet::new();
        mark_self_referential_distribution(&manifest, &declared, &mut used);
        assert!(used.contains("self-extra"));
    }
}
