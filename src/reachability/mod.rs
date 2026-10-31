//! Reachability analysis (pipeline step 9).

mod bfs;
mod build;
mod config_names;
mod error;
mod module_index;
mod trace;
mod types;

pub(crate) use bfs::module_and_parents;
pub use build::{analyze_reachability, apply_public_surface};
pub(crate) use build::{
    analyze_reachability_with_pytest, apply_distributed_surfaces, apply_member_surfaces,
};
pub(crate) use config_names::apply_user_selected;
pub(crate) use error::ReachabilityError;
pub(crate) use module_index::ModuleIndex;
pub use trace::trace_to_file;
pub(crate) use types::UnreachableFile;
pub use types::{ReachabilityReport, TracePath, TraceStep, UsedModule};
