//! First-party module name → file index.

use std::collections::HashMap;

use crate::graph::{FileId, ProjectGraph};
use crate::resolver::PytestImportPaths;
use crate::sources::{DiscoveredSources, path_to_module};

/// Maps dotted module names to project files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleIndex {
    module_to_file: HashMap<String, FileId>,
    path_to_file: HashMap<String, FileId>,
    pytest: PytestImportPaths,
}

impl ModuleIndex {
    /// Build a module index from discovered sources and the project graph.
    #[must_use]
    pub(crate) fn build(graph: &ProjectGraph, sources: &DiscoveredSources) -> Self {
        let mut module_to_file = HashMap::new();
        let mut path_to_file = HashMap::new();
        for (file_id, file) in graph.files() {
            if let Some(module) = path_to_module(&file.path, &sources.layout) {
                module_to_file.entry(module).or_insert(file_id);
            }
            path_to_file.insert(file.path.clone(), file_id);
        }
        Self {
            module_to_file,
            path_to_file,
            pytest: PytestImportPaths::build(sources),
        }
    }

    /// Resolve a dotted module name to a first-party file id.
    #[must_use]
    pub(crate) fn resolve(&self, module: &str) -> Option<FileId> {
        self.module_to_file.get(module).copied()
    }

    /// Resolve `module` as imported from the file at `from_path`: pytest's
    /// `sys.path` entries for a test file come first, as prepend mode puts
    /// them ahead of everything else; a module beside a script comes last.
    #[must_use]
    pub(crate) fn resolve_from(&self, from_path: &str, module: &str) -> Option<FileId> {
        let file_at = |path: &str| self.path_to_file.get(path).copied();
        self.pytest
            .resolve(from_path, module)
            .and_then(file_at)
            .or_else(|| self.resolve(module))
            .or_else(|| {
                self.pytest
                    .resolve_sibling(from_path, module)
                    .and_then(file_at)
            })
    }

    /// Modules strictly inside package `prefix`, sorted by name.
    #[must_use]
    pub(crate) fn under(&self, prefix: &str) -> Vec<(&str, FileId)> {
        let mut modules: Vec<_> = self
            .module_to_file
            .iter()
            .filter(|(module, _)| {
                module
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
            })
            .map(|(module, file_id)| (module.as_str(), *file_id))
            .collect();
        modules.sort_unstable_by_key(|(module, _)| *module);
        modules
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
                package_root: "src".to_owned(),
                packages: vec!["acme".to_owned()],
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
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
        assert_eq!(
            updated.under("acme"),
            vec![("acme.bar", added), ("acme.foo", file_id)]
        );
        assert_eq!(updated.under("acme.foo"), []);
        assert_eq!(updated.under("acm"), []);
    }
}
