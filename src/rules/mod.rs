//! Rule types and issue candidates shared across pipeline steps 10–12.

mod chk001;
mod context;
pub(crate) use context::{DependencyRuleContext, RuleContext};
pub mod deps;
pub mod emit;
mod filter;
pub(crate) use filter::counts_toward_exit;
mod ignore;
pub mod metadata;
mod severity;
pub mod symbols;
mod types;

pub use deps::reconcile_dependencies;
pub use emit::{emit_issues, emit_issues_with_resolution, explain_issue};
pub use metadata::{default_rule_severity, rule_help_text, rule_help_uri, rule_title};
pub use symbols::{SymbolId, SymbolReport, analyze_symbols};
pub use types::{
    DependencyReport, ExplainData, Issue, IssueCandidate, IssueLocation, IssueReport, IssueSubject,
    IssueSummary, Origin, ReconcileDiagnostic, RuleId, Severity, SuppressReason, SuppressedIssue,
    WorkspaceDependencyBoundary, issue_fingerprint, issue_stable_target,
};
