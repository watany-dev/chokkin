//! Symbol usage analysis (pipeline step 11).

mod analyze;
mod exports;
mod external;
mod graph;
mod types;

pub use analyze::analyze_symbols;
pub use graph::SymbolId;
pub use types::SymbolReport;
