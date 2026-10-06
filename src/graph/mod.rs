//! Project reachability graph (Phase 0 skeleton).

mod build;
mod edges;
mod error;
mod types;

pub use build::build_graph_skeleton;
pub use edges::add_parsed_imports;
pub use error::GraphError;
#[cfg(test)]
pub(crate) use types::FileNode;
pub(crate) use types::{FileId, ModuleId};
pub use types::{GraphEdge, ModuleOrigin, ProjectGraph};
