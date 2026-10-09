//! Symbol usage analysis (pipeline step 11).

mod analyze;
mod conventions;
mod exports;
mod external;
mod graph;
mod public;

pub use analyze::analyze_with_context;
