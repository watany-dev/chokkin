//! First-party module name → file index.

use std::collections::HashMap;

use crate::graph::{FileId, ProjectGraph};
use crate::sources::{DiscoveredSources, LayoutInfo, ProjectLayout};

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

/// Infer a dotted module name from a root-relative `.py` path.
#[must_use]
pub fn path_to_module(path: &str, layout: &LayoutInfo) -> Option<String> {
    let stem = path.strip_suffix(".py")?;
    let module_path = stem.strip_suffix("/__init__").unwrap_or(stem);

    match layout.layout {
        ProjectLayout::Src => module_path
            .strip_prefix("src/")
            .map(|rest| rest.replace('/', ".")),
        ProjectLayout::Flat => flat_module_name(module_path, layout),
        ProjectLayout::Unknown => module_path
            .strip_prefix("src/")
            .map(|rest| rest.replace('/', "."))
            .or_else(|| flat_module_name(module_path, layout)),
    }
}

fn flat_module_name(path: &str, layout: &LayoutInfo) -> Option<String> {
    for package in &layout.packages {
        if path == *package {
            return Some(package.clone());
        }
        if let Some(suffix) = path.strip_prefix(&format!("{package}/")) {
            return Some(format!("{package}.{}", suffix.replace('/', ".")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::{FileNode, ProjectGraph};
    use crate::sources::{FileContext, FileKind};

    #[test]
    fn path_to_module_src_layout() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        assert_eq!(
            path_to_module("src/acme/api/routes.py", &layout),
            Some("acme.api.routes".to_owned())
        );
        assert_eq!(
            path_to_module("src/acme/__init__.py", &layout),
            Some("acme".to_owned())
        );
    }

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
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let index = ModuleIndex::build(&graph, &sources);
        assert_eq!(index.resolve("acme.foo"), Some(file_id));
    }
}
