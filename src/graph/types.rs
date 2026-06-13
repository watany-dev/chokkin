//! Project graph node and edge types.

use std::collections::HashMap;

use indexmap::IndexMap;

use crate::discovery::ProjectRoot;
use crate::manifest::{DeclaredDependency, DependencyContext, DependencyOrigin};
use crate::sources::{FileContext, FileKind};

/// Stable identifier for a project file node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

/// Stable identifier for a logical Python module node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub u32);

/// Stable identifier for a declared distribution node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DistributionId(pub u32);

/// How a module node was classified (refined in Step 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleOrigin {
    /// First-party package under the project root.
    FirstParty,
    /// Standard library (Step 7).
    Stdlib,
    /// Third-party distribution (Step 7).
    ThirdParty,
    /// Not yet classified.
    Unknown,
}

/// A Python file participating in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileNode {
    /// Root-relative path using `/` separators.
    pub path: String,
    /// Assigned file context from source discovery.
    pub context: FileContext,
    /// Python source or stub kind.
    pub kind: FileKind,
}

/// A logical Python module (dotted name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleNode {
    /// Normalized dotted name without a leading dot.
    pub name: String,
    /// Classification origin.
    pub origin: ModuleOrigin,
}

/// A declared package distribution from manifest sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributionNode {
    /// PEP 508 normalized distribution name.
    pub name: String,
    /// Declaration contexts merged from duplicate records.
    pub contexts: Vec<DependencyContext>,
}

/// Graph edges accumulated during pipeline steps 3–6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphEdge {
    /// A file imports a module at the given 1-based line.
    FileImportsModule {
        /// Source file.
        file: FileId,
        /// Imported module.
        module: ModuleId,
        /// 1-based line number.
        line: u32,
    },
    /// Manifest metadata declares a distribution.
    ManifestDeclaresDistribution {
        /// Declared distribution.
        distribution: DistributionId,
        /// Source location in a manifest file.
        source: DependencyOrigin,
    },
}

/// Project-wide reachability graph (skeleton in Phase 0).
#[allow(clippy::partial_pub_fields)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGraph {
    /// Project root from discovery.
    pub root: ProjectRoot,
    files: IndexMap<FileId, FileNode>,
    modules: IndexMap<ModuleId, ModuleNode>,
    distributions: IndexMap<DistributionId, DistributionNode>,
    edges: Vec<GraphEdge>,
    path_to_file: HashMap<String, FileId>,
    name_to_module: HashMap<String, ModuleId>,
    name_to_distribution: HashMap<String, DistributionId>,
    next_file_id: u32,
    next_module_id: u32,
    next_distribution_id: u32,
}

impl ProjectGraph {
    /// Creates an empty graph for `root`.
    #[must_use]
    pub fn new(root: ProjectRoot) -> Self {
        Self {
            root,
            files: IndexMap::new(),
            modules: IndexMap::new(),
            distributions: IndexMap::new(),
            edges: Vec::new(),
            path_to_file: HashMap::new(),
            name_to_module: HashMap::new(),
            name_to_distribution: HashMap::new(),
            next_file_id: 0,
            next_module_id: 0,
            next_distribution_id: 0,
        }
    }

    /// Returns all graph edges.
    #[must_use]
    pub fn edges(&self) -> &[GraphEdge] {
        &self.edges
    }

    /// Returns the number of registered files.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Returns the number of registered modules.
    #[must_use]
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// Returns the number of registered distributions.
    #[must_use]
    pub fn distribution_count(&self) -> usize {
        self.distributions.len()
    }

    /// Looks up a file id by root-relative path.
    #[must_use]
    pub fn file_id(&self, path: &str) -> Option<FileId> {
        self.path_to_file.get(path).copied()
    }

    /// Registers a file node, returning its stable id.
    ///
    /// # Errors
    ///
    /// Returns [`super::GraphError::DuplicateFile`] when `path` is already registered.
    pub fn intern_file(&mut self, node: FileNode) -> Result<FileId, super::GraphError> {
        if self.path_to_file.contains_key(&node.path) {
            return Err(super::GraphError::DuplicateFile { path: node.path });
        }
        let id = FileId(self.next_file_id);
        self.next_file_id = self.next_file_id.saturating_add(1);
        self.path_to_file.insert(node.path.clone(), id);
        self.files.insert(id, node);
        Ok(id)
    }

    /// Registers a module node, returning its stable id (reuses existing names).
    pub fn intern_module(&mut self, name: String, origin: ModuleOrigin) -> ModuleId {
        if let Some(id) = self.name_to_module.get(&name) {
            return *id;
        }
        let id = ModuleId(self.next_module_id);
        self.next_module_id = self.next_module_id.saturating_add(1);
        self.name_to_module.insert(name.clone(), id);
        self.modules.insert(id, ModuleNode { name, origin });
        id
    }

    /// Registers a distribution from manifest metadata.
    pub fn intern_distribution(&mut self, dependency: &DeclaredDependency) -> DistributionId {
        if let Some(id) = self.name_to_distribution.get(&dependency.name) {
            if let Some(node) = self.distributions.get_mut(id)
                && !node.contexts.contains(&dependency.context)
            {
                node.contexts.push(dependency.context.clone());
            }
            return *id;
        }
        let id = DistributionId(self.next_distribution_id);
        self.next_distribution_id = self.next_distribution_id.saturating_add(1);
        self.name_to_distribution
            .insert(dependency.name.clone(), id);
        self.distributions.insert(
            id,
            DistributionNode {
                name: dependency.name.clone(),
                contexts: vec![dependency.context.clone()],
            },
        );
        id
    }

    /// Appends an edge to the graph.
    pub fn push_edge(&mut self, edge: GraphEdge) {
        self.edges.push(edge);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};

    fn sample_root() -> ProjectRoot {
        ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        }
    }

    #[test]
    fn intern_file_returns_stable_id() {
        let mut graph = ProjectGraph::new(sample_root());
        let id = graph
            .intern_file(FileNode {
                path: "src/app.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("first insert");
        assert_eq!(graph.file_id("src/app.py"), Some(id));
    }

    #[test]
    fn duplicate_file_is_error() {
        let mut graph = ProjectGraph::new(sample_root());
        let node = FileNode {
            path: "src/app.py".to_owned(),
            context: FileContext::Runtime,
            kind: FileKind::Python,
        };
        graph.intern_file(node.clone()).expect("first insert");
        assert!(matches!(
            graph.intern_file(node),
            Err(crate::graph::GraphError::DuplicateFile { .. })
        ));
    }
}
