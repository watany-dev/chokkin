//! Reachability analysis (pipeline step 9).

mod bfs;
mod build;
mod error;
mod module_index;
mod trace;
mod types;

pub(crate) use build::apply_member_surfaces;
pub use build::{analyze_reachability, apply_public_surface};
pub(crate) use error::ReachabilityError;
pub(crate) use module_index::ModuleIndex;
pub use trace::trace_to_file;
pub(crate) use types::UnreachableFile;
pub use types::{ReachabilityReport, TracePath, TraceStep, UsedModule};
