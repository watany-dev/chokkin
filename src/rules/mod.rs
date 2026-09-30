//! Rule types and issue candidates shared across pipeline steps 10–12.

mod chk001;
mod context;
pub use context::{DependencyRuleContext, RuleContext};
pub mod deps;
pub mod emit;
mod filter;
mod ignore;
pub mod metadata;
mod severity;
pub mod symbols;
mod types;

pub use emit::{emit_issues, explain_issue};
pub use metadata::{default_rule_severity, rule_help_text, rule_help_uri, rule_title};
pub use types::{
    DependencyReport, ExplainData, Issue, IssueCandidate, IssueLocation, IssueReport, IssueSubject,
    IssueSummary, Origin, RuleId, Severity, SuppressReason, SuppressedIssue,
    WorkspaceDependencyBoundary, issue_fingerprint, issue_stable_target,
};
