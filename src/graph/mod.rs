//! Project reachability graph (Phase 0 skeleton).

mod build;
mod edges;
mod error;
mod types;

pub use build::build_graph_skeleton;
pub use edges::add_parsed_imports;
pub use error::GraphError;
pub use types::{
    DistributionId, DistributionNode, EntryId, EntryNode, FileId, FileNode, GraphEdge, ModuleId,
    ModuleNode, ModuleOrigin, ProjectGraph,
};
