//! Symbol usage analysis types (pipeline step 11).

use indexmap::IndexSet;

use crate::rules::types::IssueCandidate;

use super::graph::SymbolId;

/// Output of pipeline step 11.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SymbolReport {
    /// Issue candidates for Step 12.
    pub candidates: Vec<IssueCandidate>,
    /// Public symbols considered in reachable modules.
    pub symbol_count: u32,
    /// Symbols marked as externally used (decorators, entry points, plugins).
    pub external_symbols: IndexSet<SymbolId>,
}
