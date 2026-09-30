//! Build graph nodes from manifest and discovered sources.

use crate::manifest::LoadedManifest;
use crate::sources::DiscoveredSources;

use super::error::GraphError;
use super::types::{FileNode, ModuleOrigin, ProjectGraph};

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
        graph.intern_distribution(&dependency.name);
    }

    Ok(graph)
}
