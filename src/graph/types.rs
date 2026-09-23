//! Project graph node and edge types.

use std::collections::HashMap;

use crate::discovery::ProjectRoot;
use crate::manifest::DependencyOrigin;
use crate::plugins::ReferenceOrigin;
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

/// Stable identifier for an entry root node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(pub u32);

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

/// How one project file reaches another during reachability analysis (step 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileReachVia {
    /// Static or resolved import edge.
    Import,
    /// Plugin configuration module reference.
    PluginReference,
    /// Literal dynamic import.
    DynamicImport,
}

/// Graph edges accumulated during pipeline steps 3–9.
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
        /// The site is a literal `importlib.import_module` / `__import__` call,
        /// not an `import` statement.
        dynamic: bool,
    },
    /// Manifest metadata declares a distribution.
    ManifestDeclaresDistribution {
        /// Declared distribution.
        distribution: DistributionId,
        /// Source location in a manifest file.
        source: DependencyOrigin,
    },
    /// A distribution provides an importable module (Step 7).
    DistributionProvidesModule {
        /// Declared distribution.
        distribution: DistributionId,
        /// Provided module.
        module: ModuleId,
    },
    /// An entry root reaches a project file (Step 8).
    EntryReachesFile {
        /// Entry root node.
        entry: EntryId,
        /// Target file.
        file: FileId,
    },
    /// A project file reaches another via import resolution (Step 9).
    FileReachesFile {
        /// Source file.
        from: FileId,
        /// Target file.
        to: FileId,
        /// How the reach was discovered.
        via: FileReachVia,
    },
    /// Plugin configuration references a module (Step 9).
    ConfigReferenceUsesModule {
        /// Discovery origin.
        origin: ReferenceOrigin,
        /// Referenced module.
        module: ModuleId,
    },
}

/// Project-wide reachability graph (skeleton in Phase 0).
#[allow(clippy::partial_pub_fields)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGraph {
    /// Project root from discovery.
    pub root: ProjectRoot,
    files: Vec<FileNode>,
    modules: Vec<ModuleNode>,
    edges: Vec<GraphEdge>,
    path_to_file: HashMap<String, FileId>,
    name_to_module: HashMap<String, ModuleId>,
    name_to_distribution: HashMap<String, DistributionId>,
    entry_count: usize,
}

/// Next sequential id for a table holding `len` nodes.
fn next_id(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

impl ProjectGraph {
    /// Creates an empty graph for `root`.
    #[must_use]
    pub fn new(root: ProjectRoot) -> Self {
        Self {
            root,
            files: Vec::new(),
            modules: Vec::new(),
            edges: Vec::new(),
            path_to_file: HashMap::new(),
            name_to_module: HashMap::new(),
            name_to_distribution: HashMap::new(),
            entry_count: 0,
        }
    }

    /// Returns all graph edges.
    #[must_use]
    pub fn edges(&self) -> &[GraphEdge] {
        &self.edges
    }

    /// Returns a registered file node.
    #[must_use]
    pub fn file(&self, id: FileId) -> Option<&FileNode> {
        usize::try_from(id.0).ok().and_then(|i| self.files.get(i))
    }

    /// Returns a registered module node.
    #[must_use]
    pub fn module(&self, id: ModuleId) -> Option<&ModuleNode> {
        usize::try_from(id.0).ok().and_then(|i| self.modules.get(i))
    }

    /// Iterates registered files in insertion order.
    pub fn files(&self) -> impl Iterator<Item = (FileId, &FileNode)> {
        self.files
            .iter()
            .enumerate()
            .map(|(index, node)| (FileId(next_id(index)), node))
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
        self.name_to_distribution.len()
    }

    /// Returns the number of registered entry roots.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Looks up a file id by root-relative path.
    #[must_use]
    pub fn file_id(&self, path: &str) -> Option<FileId> {
        self.path_to_file.get(path).copied()
    }

    /// Looks up a module id by dotted name.
    #[must_use]
    pub fn module_id(&self, name: &str) -> Option<ModuleId> {
        self.name_to_module.get(name).copied()
    }

    /// Updates the origin classification for an existing module node.
    ///
    /// Origins are merged monotonically so repeated writes for the same module
    /// name never downgrade a resolved classification to [`ModuleOrigin::Unknown`].
    pub fn set_module_origin(&mut self, module: ModuleId, origin: ModuleOrigin) {
        if let Some(node) = usize::try_from(module.0)
            .ok()
            .and_then(|i| self.modules.get_mut(i))
        {
            node.origin = merge_module_origin(node.origin, origin);
        }
    }

    /// Looks up a distribution id by normalized name.
    #[must_use]
    pub fn distribution_id(&self, name: &str) -> Option<DistributionId> {
        self.name_to_distribution.get(name).copied()
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
        let id = FileId(next_id(self.files.len()));
        self.path_to_file.insert(node.path.clone(), id);
        self.files.push(node);
        Ok(id)
    }

    /// Registers a module node, returning its stable id (reuses existing names).
    pub fn intern_module(&mut self, name: String, origin: ModuleOrigin) -> ModuleId {
        if let Some(id) = self.name_to_module.get(&name) {
            return *id;
        }
        let id = ModuleId(next_id(self.modules.len()));
        self.name_to_module.insert(name.clone(), id);
        self.modules.push(ModuleNode { name, origin });
        id
    }

    /// Registers a distribution by normalized name, reusing an existing id.
    pub fn intern_distribution(&mut self, name: &str) -> DistributionId {
        if let Some(id) = self.name_to_distribution.get(name) {
            return *id;
        }
        let id = DistributionId(next_id(self.name_to_distribution.len()));
        self.name_to_distribution.insert(name.to_owned(), id);
        id
    }

    /// Appends an edge to the graph.
    pub fn push_edge(&mut self, edge: GraphEdge) {
        self.edges.push(edge);
    }

    /// Registers an entry root, returning its stable id.
    pub fn intern_entry(&mut self) -> EntryId {
        let id = EntryId(next_id(self.entry_count));
        self.entry_count = self.entry_count.saturating_add(1);
        id
    }
}

/// Merge two module origins, keeping the more specific classification.
fn merge_module_origin(current: ModuleOrigin, candidate: ModuleOrigin) -> ModuleOrigin {
    fn rank(origin: ModuleOrigin) -> u8 {
        match origin {
            ModuleOrigin::Stdlib | ModuleOrigin::FirstParty => 3,
            ModuleOrigin::ThirdParty => 2,
            ModuleOrigin::Unknown => 1,
        }
    }

    if rank(candidate) > rank(current) {
        candidate
    } else {
        current
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    #[test]
    fn merge_prefers_resolved_over_unknown() {
        assert_eq!(
            merge_module_origin(ModuleOrigin::Unknown, ModuleOrigin::ThirdParty),
            ModuleOrigin::ThirdParty
        );
        assert_eq!(
            merge_module_origin(ModuleOrigin::ThirdParty, ModuleOrigin::Unknown),
            ModuleOrigin::ThirdParty
        );
    }

    #[test]
    fn merge_prefers_first_party_over_third_party() {
        assert_eq!(
            merge_module_origin(ModuleOrigin::ThirdParty, ModuleOrigin::FirstParty),
            ModuleOrigin::FirstParty
        );
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
