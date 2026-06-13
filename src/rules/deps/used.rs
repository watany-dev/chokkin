//! Build the set of used distributions from imports, plugins, and binaries.

use std::collections::{BTreeMap, HashSet};

use indexmap::IndexSet;

use crate::graph::{ModuleOrigin, ProjectGraph};
use crate::manifest::LoadedManifest;
use crate::plugins::PluginHints;
use crate::reachability::ReachabilityReport;
use crate::resolver::ResolutionIndex;
use crate::sources::DiscoveredSources;

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
    manifest.sources.uv_lock || !resolution.transitive.edges.is_empty()
}

/// Distributions used by reachable imports, plugin refs, and binaries.
#[allow(clippy::too_many_arguments)]
pub(super) fn collect_used_distributions(
    resolution: &ResolutionIndex,
    reachability: &ReachabilityReport,
    plugins: &PluginHints,
    sources: &DiscoveredSources,
    graph: &ProjectGraph,
    binary_resolutions: &BTreeMap<String, String>,
) -> IndexSet<String> {
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
        let _ = sources;
        used.insert(distribution.clone());
    }

    for usage in plugins.all_binary_usages() {
        if let Some(distribution) = binary_resolutions.get(&usage.binary) {
            used.insert(distribution.clone());
        }
    }

    used
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
    fn detects_lockfile_from_uv_lock_flag() {
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
                uv_lock: true,
                ..ManifestSources::default()
            },
            warnings: Vec::new(),
        };
        let resolution = ResolutionIndex::empty();
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
            let mut report = ReachabilityReport::empty();
            report.reachable.insert(file_id);
            report
        };
        let resolution = ResolutionIndex {
            imports: vec![ResolvedImport {
                import_root: "yaml".to_owned(),
                full_module: "yaml".to_owned(),
                file: "src/app.py".to_owned(),
                line: 1,
                context: ImportContext::Runtime,
                origin: ModuleOrigin::ThirdParty,
                distribution: Some("pyyaml".to_owned()),
                confidence: ResolveConfidence::Certain,
            }],
            warnings: Vec::new(),
            transitive: TransitiveIndex::empty(),
            binary_resolutions: BTreeMap::new(),
        };
        let sources = DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let used = collect_used_distributions(
            &resolution,
            &reachable,
            &PluginHints {
                contributions: Vec::new(),
                warnings: Vec::new(),
            },
            &sources,
            &graph,
            &BTreeMap::new(),
        );
        assert!(used.contains("pyyaml"));
    }
}
