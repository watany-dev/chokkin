//! Rule types and issue candidates shared across pipeline steps 10–12.

mod chk001;
mod context;
pub use context::{DependencyRuleContext, RuleContext};
pub(crate) mod deps;
pub(crate) mod emit;
mod filter;
mod gaps;
mod ignore;
pub(crate) mod metadata;
mod severity;
pub(crate) mod symbols;
mod types;

pub use emit::{emit_issues, explain_issue};
pub(crate) use gaps::GapKind;
pub use gaps::{Gap, MemberManifest, collect_gaps};
#[cfg(test)]
pub(crate) use metadata::default_rule_severity;
pub(crate) use metadata::{rule_help_text, rule_help_uri, rule_title};
#[cfg(test)]
pub(crate) use types::ExplainData;
pub use types::{
    DependencyReport, Issue, IssueCandidate, IssueLocation, IssueReport, IssueSubject,
    IssueSummary, Origin, RuleId, Severity, SuppressReason, SuppressedIssue,
    WorkspaceDependencyBoundary, issue_fingerprint,
};
pub(crate) use types::{import_site, issue_stable_target};
