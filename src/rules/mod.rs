//! Rule types and issue candidates shared across pipeline steps 10–12.

pub mod deps;
mod types;

pub use deps::reconcile_dependencies;
pub use types::{
    DependencyReport, ExplainData, IssueCandidate, IssueSubject, Origin, ReconcileDiagnostic,
    RuleId, Severity,
};
