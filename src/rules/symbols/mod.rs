//! Symbol usage analysis (pipeline step 11).

mod analyze;
mod exports;
mod external;
mod graph;

pub use analyze::analyze_with_context;
