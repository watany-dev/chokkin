//! Rule types and issue candidates shared across pipeline steps 10–12.

mod chk001;
pub mod deps;
pub mod emit;
mod filter;
mod ignore;
pub mod symbols;
mod types;

pub use deps::reconcile_dependencies;
pub use emit::{emit_issues, explain_issue};
pub use symbols::{SymbolId, SymbolReport, analyze_symbols};
pub use types::{
    DependencyReport, ExplainData, Issue, IssueCandidate, IssueLocation, IssueReport, IssueSubject,
    IssueSummary, Origin, ReconcileDiagnostic, RuleId, Severity, SuppressReason, SuppressedIssue,
};
