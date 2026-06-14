//! Rule types and issue candidates shared across pipeline steps 10–12.

pub mod deps;
pub mod symbols;
mod types;

pub use deps::reconcile_dependencies;
pub use symbols::{SymbolId, SymbolReport, analyze_symbols};
pub use types::{
    DependencyReport, ExplainData, IssueCandidate, IssueSubject, Origin, ReconcileDiagnostic,
    RuleId, Severity,
};
