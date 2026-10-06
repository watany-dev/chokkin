//! Rule types and issue candidates shared across pipeline steps 10–12.

mod chk001;
mod context;
pub use context::{DependencyRuleContext, RuleContext};
pub(crate) mod deps;
pub(crate) mod emit;
mod filter;
mod ignore;
pub(crate) mod metadata;
mod severity;
pub(crate) mod symbols;
mod types;

pub use emit::{emit_issues, explain_issue};
#[cfg(test)]
pub(crate) use metadata::default_rule_severity;
pub(crate) use metadata::{rule_help_text, rule_help_uri, rule_title};
#[cfg(test)]
pub(crate) use types::ExplainData;
pub(crate) use types::issue_stable_target;
pub use types::{
    DependencyReport, Issue, IssueCandidate, IssueLocation, IssueReport, IssueSubject,
    IssueSummary, Origin, RuleId, Severity, SuppressReason, SuppressedIssue,
    WorkspaceDependencyBoundary, issue_fingerprint,
};
