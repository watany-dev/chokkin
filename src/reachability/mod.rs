//! Reachability analysis (pipeline step 9).

mod bfs;
mod build;
mod error;
mod module_index;
mod trace;
mod types;

pub use build::{analyze_reachability, analyze_reachability_with_cache};
pub use error::ReachabilityError;
pub use module_index::{ModuleIndex, path_to_module};
pub use trace::trace_to_file;
pub use types::{
    ReachabilityReport, TracePath, TraceStep, UnreachableFile, UnreachableReason, UsedModule,
};
