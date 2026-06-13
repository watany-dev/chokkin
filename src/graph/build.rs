//! Build graph nodes from manifest and discovered sources.

use crate::manifest::LoadedManifest;
use crate::sources::{DiscoveredSources, FileKind};

use super::error::GraphError;
use super::types::{FileNode, GraphEdge, ModuleOrigin, ProjectGraph};

/// Initialize graph file and distribution nodes from pipeline steps 3–4.
///
/// # Errors
///
/// Returns [`GraphError`] when duplicate file paths are encountered.
pub fn build_graph_skeleton(
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
) -> Result<ProjectGraph, GraphError> {
    let mut graph = ProjectGraph::new(sources.root.clone());

    for file in &sources.files {
        graph.intern_file(FileNode {
            path: file.path.clone(),
            context: file.context,
            kind: file.kind,
        })?;
    }

    for package in &sources.layout.packages {
        graph.intern_module(package.clone(), ModuleOrigin::FirstParty);
    }

    for dependency in &manifest.dependencies {
        if dependency.opaque {
            continue;
        }
        let distribution_id = graph.intern_distribution(dependency);
        graph.push_edge(GraphEdge::ManifestDeclaresDistribution {
            distribution: distribution_id,
            source: dependency.origin.clone(),
        });
    }

    // Stub files are indexed but not parsed in Phase 0.
    let _ = FileKind::Stub;

    Ok(graph)
}
