//! First-party module name → file index.

use std::collections::HashMap;

use crate::graph::{FileId, ProjectGraph};
use crate::sources::{DiscoveredSources, path_to_module};

/// Maps dotted module names to project files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleIndex {
    module_to_file: HashMap<String, FileId>,
}

impl ModuleIndex {
    /// Build a module index from discovered sources and the project graph.
    #[must_use]
    pub fn build(graph: &ProjectGraph, sources: &DiscoveredSources) -> Self {
        let mut module_to_file = HashMap::new();
        for (file_id, file) in graph.files() {
            if let Some(module) = path_to_module(&file.path, &sources.layout) {
                module_to_file.entry(module).or_insert(file_id);
            }
        }
        Self { module_to_file }
    }

    /// Resolve a dotted module name to a first-party file id.
    #[must_use]
    pub fn resolve(&self, module: &str) -> Option<FileId> {
        self.module_to_file.get(module).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::{FileNode, ProjectGraph};
    use crate::sources::{FileContext, FileKind, LayoutInfo, ProjectLayout};

    #[test]
    fn module_index_resolves_registered_file() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        });
        let file_id = graph
            .intern_file(FileNode {
                path: "src/acme/foo.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("file");
        let sources = DiscoveredSources {
            root: graph.root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let index = ModuleIndex::build(&graph, &sources);
        assert_eq!(index.resolve("acme.foo"), Some(file_id));
        let added = graph
            .intern_file(FileNode {
                path: "src/acme/bar.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("new file");
        let updated = ModuleIndex::build(&graph, &sources);
        assert_eq!(updated.resolve("acme.bar"), Some(added));
    }
}
