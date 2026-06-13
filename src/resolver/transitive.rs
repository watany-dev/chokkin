//! Lockfile transitive dependency index.

use crate::manifest::LoadedManifest;

use super::types::TransitiveIndex;

/// Build transitive index from manifest lockfile data.
#[must_use]
pub fn build_transitive_index(manifest: &LoadedManifest) -> TransitiveIndex {
    TransitiveIndex {
        edges: manifest.lockfile.edges.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::manifest::{LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata};
    use std::collections::BTreeMap;

    #[test]
    fn copies_lockfile_edges() {
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
            lockfile: LockfileGraph {
                edges: BTreeMap::from([("app".to_owned(), vec!["requests".to_owned()])]),
                requires_python: None,
            },
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        };
        let index = build_transitive_index(&manifest);
        assert_eq!(
            index.edges.get("app").map(Vec::as_slice),
            Some(["requests".to_owned()].as_slice())
        );
    }
}
