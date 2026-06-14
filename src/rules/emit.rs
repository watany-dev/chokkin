//! Issue emission orchestration (pipeline step 12).

use std::collections::BTreeMap;

use crate::ExitStatus;
use crate::config::{RuntimeOverrides, YokeiConfig};
use crate::entry::ResolvedMode;
use crate::parser::ParseSummary;
use crate::reachability::ReachabilityReport;
use crate::rules::symbols::SymbolReport;
use crate::rules::types::DependencyReport;
use crate::rules::types::{
    Issue, IssueCandidate, IssueLocation, IssueReport, IssueSubject, IssueSummary, Origin,
    SuppressedIssue, subject_sort_key,
};

use super::filter::{
    counts_toward_exit, effective_confidence_floor, passes_confidence_filter, passes_rule_filter,
};
use super::ignore::IgnoreMatcher;
use super::types::RuleId;
use super::yok001::yok001_candidates;

/// Merge candidates, apply ignore/confidence filters, and compute exit status.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn emit_issues(
    unreachable: &ReachabilityReport,
    deps: &DependencyReport,
    symbols: &SymbolReport,
    parse: &ParseSummary,
    config: &YokeiConfig,
    overrides: &RuntimeOverrides,
    mode: &ResolvedMode,
) -> IssueReport {
    let strict = overrides.strict.unwrap_or(false);
    let matcher = IgnoreMatcher::build(config, parse);
    let confidence_floor = effective_confidence_floor(config, overrides, strict);

    let mut candidates = yok001_candidates(&unreachable.unreachable, mode);
    candidates.extend(deps.candidates.clone());
    candidates.extend(symbols.candidates.clone());

    candidates.sort_by(|left, right| {
        left.rule
            .as_code()
            .cmp(right.rule.as_code())
            .then_with(|| subject_sort_key(&left.subject).cmp(&subject_sort_key(&right.subject)))
    });

    let mut issues = Vec::new();
    let mut suppressed = Vec::new();

    for candidate in candidates {
        let ignore = matcher.matches_candidate(&candidate);
        let issue = candidate_to_issue(candidate);

        if let Some(reason) = ignore.reason() {
            suppressed.push(SuppressedIssue { issue, reason });
            continue;
        }

        if !passes_confidence_filter(&issue, confidence_floor) {
            continue;
        }
        if !passes_rule_filter(&issue, overrides) {
            continue;
        }

        issues.push(issue);
    }

    let summary = build_summary(&issues);
    let exit_status = compute_exit_status(&issues, overrides, strict);

    IssueReport {
        issues,
        suppressed,
        summary,
        exit_status,
    }
}

/// Render explain text for a selector such as `YOK002:boto3`.
#[must_use]
pub fn explain_issue(report: &IssueReport, selector: &str) -> Option<String> {
    let (code, subject_key) = selector.split_once(':')?;
    let rule = RuleId::parse_code(code)?;
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.rule == rule && subject_key_matches(&issue.subject, subject_key))?;
    Some(format_explain(issue))
}

fn subject_key_matches(subject: &IssueSubject, key: &str) -> bool {
    match subject {
        IssueSubject::File { path } => path == key,
        IssueSubject::Distribution { name } | IssueSubject::Binary { name } => name == key,
        IssueSubject::Symbol { module, name } => format!("{module}:{name}") == key || name == key,
        IssueSubject::Import { module, file, line } => {
            format!("{file}:{line}:{module}") == key || module == key
        },
    }
}

fn format_explain(issue: &Issue) -> String {
    let mut lines = Vec::new();
    if let Some(explain) = &issue.explain {
        lines.push(explain.summary.clone());
        lines.extend(explain.details.clone());
    } else {
        lines.push(issue.message.clone());
    }
    lines.join("\n")
}

fn candidate_to_issue(candidate: IssueCandidate) -> Issue {
    let location = location_from_candidate(&candidate);
    let explain = if candidate.explain.summary.is_empty() && candidate.explain.details.is_empty() {
        None
    } else {
        Some(candidate.explain)
    };

    Issue {
        rule: candidate.rule,
        severity: candidate.severity,
        confidence: candidate.confidence,
        message: candidate.message,
        location,
        subject: candidate.subject,
        explain,
    }
}

fn location_from_candidate(candidate: &IssueCandidate) -> IssueLocation {
    let mut file = None;
    let mut line = None;
    let mut manifest = None;

    for origin in &candidate.origins {
        match origin {
            Origin::Manifest(origin) => manifest = Some(origin.clone()),
            Origin::Import {
                file: import_file,
                line: import_line,
                ..
            } => {
                file = Some(import_file.clone());
                line = Some(*import_line);
            },
            Origin::Binary(origin) | Origin::Config(origin) => {
                file = Some(origin.file.clone());
                line = origin.line;
            },
        }
    }

    if file.is_none() {
        match &candidate.subject {
            IssueSubject::File { path } => file = Some(path.clone()),
            IssueSubject::Import {
                file: import_file,
                line: import_line,
                ..
            } => {
                file = Some(import_file.clone());
                line = Some(*import_line);
            },
            _ => {},
        }
    }

    IssueLocation {
        file,
        line,
        manifest,
    }
}

fn build_summary(issues: &[Issue]) -> IssueSummary {
    let mut by_rule = BTreeMap::new();
    for issue in issues {
        *by_rule.entry(issue.rule).or_insert(0) += 1;
    }
    IssueSummary {
        total: u32::try_from(issues.len()).unwrap_or(u32::MAX),
        by_rule,
    }
}

fn compute_exit_status(issues: &[Issue], overrides: &RuntimeOverrides, strict: bool) -> ExitStatus {
    if overrides.no_exit_code == Some(true) {
        return ExitStatus::Success;
    }
    if issues.iter().any(|issue| counts_toward_exit(issue, strict)) {
        ExitStatus::IssuesFound
    } else {
        ExitStatus::Success
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Confidence, ProjectMode, default_config};
    use crate::entry::ResolvedMode;
    use crate::graph::FileId;
    use crate::manifest::DependencyOrigin;
    use crate::reachability::{ReachabilityReport, UnreachableFile};
    use crate::resolver::ResolveConfidence;
    use crate::rules::symbols::SymbolReport;
    use crate::rules::types::{
        DependencyReport, ExplainData, IssueCandidate, IssueSubject, Severity,
    };

    fn resolved_app_mode() -> ResolvedMode {
        ResolvedMode {
            mode: ProjectMode::App,
            confidence: ResolveConfidence::Certain,
        }
    }

    #[test]
    fn emits_yok001_for_unreachable_file() {
        let mut report = ReachabilityReport::empty();
        report.unreachable.push(UnreachableFile {
            file: FileId(0),
            path: "src/legacy.py".to_owned(),
            reasons: vec![crate::reachability::UnreachableReason::NotReachable],
            max_confidence: Confidence::Certain,
        });

        let deps = DependencyReport::default();
        let symbols = SymbolReport::default();
        let parse = ParseSummary::empty();
        let config = default_config();

        let issues = emit_issues(
            &report,
            &deps,
            &symbols,
            &parse,
            &config,
            &RuntimeOverrides::default(),
            &resolved_app_mode(),
        );
        assert_eq!(issues.issues.len(), 1);
        assert_eq!(issues.issues[0].rule, RuleId::Yok001);
        assert_eq!(issues.exit_status, ExitStatus::IssuesFound);
    }

    #[test]
    fn no_exit_code_returns_success() {
        let mut report = ReachabilityReport::empty();
        report.unreachable.push(UnreachableFile {
            file: FileId(0),
            path: "src/legacy.py".to_owned(),
            reasons: vec![crate::reachability::UnreachableReason::NotReachable],
            max_confidence: Confidence::Certain,
        });

        let issues = emit_issues(
            &report,
            &DependencyReport::default(),
            &SymbolReport::default(),
            &ParseSummary::empty(),
            &default_config(),
            &RuntimeOverrides {
                no_exit_code: Some(true),
                ..RuntimeOverrides::default()
            },
            &resolved_app_mode(),
        );
        assert_eq!(issues.exit_status, ExitStatus::Success);
    }

    #[test]
    fn explain_issue_finds_selector() {
        let candidate = IssueCandidate {
            rule: RuleId::Yok002,
            subject: IssueSubject::Distribution {
                name: "boto3".to_owned(),
            },
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: "unused boto3".to_owned(),
            origins: vec![Origin::Manifest(DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(5),
                label: "project.dependencies[0]".to_owned(),
            })],
            explain: ExplainData {
                summary: "boto3 is declared but not used".to_owned(),
                details: vec!["declaration: project.dependencies[0]".to_owned()],
            },
        };
        let deps = DependencyReport {
            candidates: vec![candidate],
            ..DependencyReport::default()
        };
        let report = emit_issues(
            &ReachabilityReport::empty(),
            &deps,
            &SymbolReport::default(),
            &ParseSummary::empty(),
            &default_config(),
            &RuntimeOverrides::default(),
            &resolved_app_mode(),
        );
        let text = explain_issue(&report, "YOK002:boto3").expect("explain");
        assert!(text.contains("boto3 is declared but not used"));
    }
}
