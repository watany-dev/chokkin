//! Issue emission orchestration (pipeline step 12).

use std::collections::BTreeMap;

use crate::ExitStatus;
use crate::config::{ChokkinConfig, ProjectMode, RuntimeOverrides};
use crate::parser::ParseSummary;
use crate::reachability::ReachabilityReport;
use crate::resolver::ResolutionIndex;
use crate::rules::symbols::SymbolReport;
use crate::rules::types::DependencyReport;
use crate::rules::types::{
    Issue, IssueCandidate, IssueLocation, IssueReport, IssueSubject, IssueSummary, Origin,
    SuppressedIssue, sort_candidates,
};

use super::chk001::chk001_candidates;
use super::filter::{
    counts_toward_exit, effective_confidence_floor, passes_confidence_filter, passes_rule_filter,
};
use super::ignore::IgnoreMatcher;
use super::severity::apply_severity_override;
use super::types::RuleId;

/// Merge candidates, apply ignore/confidence filters, and compute exit status.
///
/// Dependency-rule ignore patterns are matched against distribution names
/// taken from `resolution` (§18).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn emit_issues(
    unreachable: &ReachabilityReport,
    deps: &DependencyReport,
    symbols: &SymbolReport,
    parse: &ParseSummary,
    config: &ChokkinConfig,
    overrides: &RuntimeOverrides,
    mode: ProjectMode,
    resolution: &ResolutionIndex,
) -> IssueReport {
    let strict = overrides.strict.unwrap_or(false);
    let matcher = IgnoreMatcher::build(config, parse, resolution);
    let confidence_floor = effective_confidence_floor(config, overrides, strict);

    let mut candidates = chk001_candidates(&unreachable.unreachable, mode);
    candidates.extend(deps.candidates.clone());
    candidates.extend(symbols.candidates.clone());

    sort_candidates(&mut candidates);

    let mut issues = Vec::new();
    let mut suppressed = Vec::new();

    for candidate in candidates {
        let Some(candidate) = apply_severity_override(candidate, config) else {
            continue;
        };
        let ignore = matcher.matches_candidate(&candidate);
        let issue = candidate_to_issue(candidate);

        if let Some(reason) = ignore {
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

/// Render explain text for a selector such as `CHK002:boto3`.
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
        IssueSubject::Import {
            module, file, line, ..
        } => format!("{file}:{line}:{module}") == key || module == key,
        IssueSubject::ScriptDistribution { script, name } => {
            format!("script:{script}:{name}") == key || name == key
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
        workspace_member: candidate.workspace_member,
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
            IssueSubject::ScriptDistribution { script, .. } => {
                file = Some(script.clone());
                line = manifest.as_ref().and_then(|origin| origin.line);
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

pub(crate) fn build_summary(issues: &[Issue]) -> IssueSummary {
    let mut by_rule = BTreeMap::new();
    for issue in issues {
        *by_rule.entry(issue.rule).or_insert(0) += 1;
    }
    IssueSummary {
        total: u32::try_from(issues.len()).unwrap_or(u32::MAX),
        by_rule,
    }
}

pub(crate) fn compute_exit_status(
    issues: &[Issue],
    overrides: &RuntimeOverrides,
    strict: bool,
) -> ExitStatus {
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
    use crate::graph::FileId;
    use crate::manifest::DependencyOrigin;
    use crate::reachability::{ReachabilityReport, UnreachableFile};
    use crate::rules::symbols::SymbolReport;
    use crate::rules::types::{
        DependencyReport, ExplainData, IssueCandidate, IssueSubject, Severity,
    };

    #[test]
    fn emits_chk001_for_unreachable_file() {
        let mut report = ReachabilityReport::default();
        report.unreachable.push(UnreachableFile {
            file: FileId(0),
            path: "src/legacy.py".to_owned(),
            max_confidence: Confidence::Certain,
        });

        let deps = DependencyReport::default();
        let symbols = SymbolReport::default();
        let parse = ParseSummary::default();
        let config = default_config();

        let issues = emit_issues(
            &report,
            &deps,
            &symbols,
            &parse,
            &config,
            &RuntimeOverrides::default(),
            ProjectMode::App,
            &ResolutionIndex::default(),
        );
        assert_eq!(issues.issues.len(), 1);
        assert_eq!(issues.issues[0].rule, RuleId::Chk001);
        assert_eq!(issues.exit_status, ExitStatus::IssuesFound);
    }

    #[test]
    fn summary_counts_issues_per_rule() {
        let issue = |rule| Issue {
            rule,
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: String::new(),
            workspace_member: None,
            location: IssueLocation {
                file: None,
                line: None,
                manifest: None,
            },
            subject: IssueSubject::File {
                path: "a.py".to_owned(),
            },
            explain: None,
        };
        let summary = build_summary(&[
            issue(RuleId::Chk001),
            issue(RuleId::Chk002),
            issue(RuleId::Chk002),
        ]);
        assert_eq!(summary.total, 3);
        assert_eq!(
            summary.by_rule,
            BTreeMap::from([(RuleId::Chk001, 1), (RuleId::Chk002, 2)])
        );
    }

    #[test]
    fn no_exit_code_returns_success() {
        let mut report = ReachabilityReport::default();
        report.unreachable.push(UnreachableFile {
            file: FileId(0),
            path: "src/legacy.py".to_owned(),
            max_confidence: Confidence::Certain,
        });

        let issues = emit_issues(
            &report,
            &DependencyReport::default(),
            &SymbolReport::default(),
            &ParseSummary::default(),
            &default_config(),
            &RuntimeOverrides {
                no_exit_code: Some(true),
                ..RuntimeOverrides::default()
            },
            ProjectMode::App,
            &ResolutionIndex::default(),
        );
        assert_eq!(issues.exit_status, ExitStatus::Success);
    }

    #[test]
    fn severity_off_skips_rule() {
        let candidate = IssueCandidate {
            rule: RuleId::Chk002,
            subject: IssueSubject::Distribution {
                name: "boto3".to_owned(),
            },
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: "unused boto3".to_owned(),
            workspace_member: None,
            origins: vec![Origin::Manifest(DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(5),
                label: "project.dependencies[0]".to_owned(),
            })],
            explain: ExplainData::default(),
        };
        let deps = DependencyReport {
            candidates: vec![candidate],
            ..DependencyReport::default()
        };
        let mut config = default_config();
        config
            .severity
            .insert("CHK002".to_owned(), crate::config::SeverityLevel::Off);
        let report = emit_issues(
            &ReachabilityReport::default(),
            &deps,
            &SymbolReport::default(),
            &ParseSummary::default(),
            &config,
            &RuntimeOverrides::default(),
            ProjectMode::App,
            &ResolutionIndex::default(),
        );
        assert!(report.issues.is_empty());
    }

    #[test]
    fn explain_issue_finds_selector() {
        let candidate = IssueCandidate {
            rule: RuleId::Chk002,
            subject: IssueSubject::Distribution {
                name: "boto3".to_owned(),
            },
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: "unused boto3".to_owned(),
            workspace_member: None,
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
            &ReachabilityReport::default(),
            &deps,
            &SymbolReport::default(),
            &ParseSummary::default(),
            &default_config(),
            &RuntimeOverrides::default(),
            ProjectMode::App,
            &ResolutionIndex::default(),
        );
        let text = explain_issue(&report, "CHK002:boto3").expect("explain");
        assert!(text.contains("boto3 is declared but not used"));
    }

    fn bare_candidate(rule: RuleId, subject: IssueSubject, summary: &str) -> IssueCandidate {
        IssueCandidate {
            rule,
            subject,
            severity: Severity::Warning,
            confidence: Confidence::Certain,
            message: String::new(),
            workspace_member: None,
            origins: Vec::new(),
            explain: ExplainData {
                summary: summary.to_owned(),
                details: Vec::new(),
            },
        }
    }

    #[test]
    fn explain_issue_selects_by_rule_and_every_subject_key_form() {
        // The script issue precedes the plain CHK002 one, so a selector that
        // ignored the subject key would pick the wrong issue.
        let issues = [
            bare_candidate(
                RuleId::Chk006,
                IssueSubject::Symbol {
                    module: "acme.utils".to_owned(),
                    name: "dead_api".to_owned(),
                },
                "symbol",
            ),
            bare_candidate(
                RuleId::Chk010,
                IssueSubject::Import {
                    module: "requets".to_owned(),
                    file: "src/app.py".to_owned(),
                    line: 3,
                    distribution: None,
                },
                "import",
            ),
            bare_candidate(
                RuleId::Chk002,
                IssueSubject::ScriptDistribution {
                    script: "scripts/tool.py".to_owned(),
                    name: "rich".to_owned(),
                },
                "script",
            ),
            bare_candidate(
                RuleId::Chk002,
                IssueSubject::Distribution {
                    name: "boto3".to_owned(),
                },
                "distribution",
            ),
            bare_candidate(
                RuleId::Chk008,
                IssueSubject::Binary {
                    name: "ruff".to_owned(),
                },
                "binary",
            ),
            bare_candidate(
                RuleId::Chk001,
                IssueSubject::File {
                    path: "src/legacy.py".to_owned(),
                },
                "file",
            ),
        ]
        .map(candidate_to_issue)
        .to_vec();
        let report = IssueReport {
            summary: build_summary(&issues),
            issues,
            suppressed: Vec::new(),
            exit_status: ExitStatus::IssuesFound,
        };

        for (selector, expected) in [
            ("CHK006:acme.utils:dead_api", Some("symbol")),
            ("CHK006:dead_api", Some("symbol")),
            ("CHK010:src/app.py:3:requets", Some("import")),
            ("CHK010:requets", Some("import")),
            ("CHK002:script:scripts/tool.py:rich", Some("script")),
            ("CHK002:rich", Some("script")),
            ("CHK002:boto3", Some("distribution")),
            ("CHK008:ruff", Some("binary")),
            ("CHK001:src/legacy.py", Some("file")),
            ("CHK003:boto3", None),
            ("CHK002:missing", None),
            ("CHK006:acme.utils:other", None),
        ] {
            assert_eq!(
                explain_issue(&report, selector).as_deref(),
                expected,
                "{selector}"
            );
        }
    }

    #[test]
    fn candidate_explain_is_dropped_only_when_summary_and_details_are_both_empty() {
        let subject = || IssueSubject::Distribution {
            name: "boto3".to_owned(),
        };
        let summary_only = bare_candidate(RuleId::Chk002, subject(), "summary");
        let mut details_only = bare_candidate(RuleId::Chk002, subject(), "");
        details_only.explain.details = vec!["detail".to_owned()];
        let neither = bare_candidate(RuleId::Chk002, subject(), "");

        assert!(candidate_to_issue(summary_only).explain.is_some());
        assert!(candidate_to_issue(details_only).explain.is_some());
        assert!(candidate_to_issue(neither).explain.is_none());
    }

    #[test]
    fn location_falls_back_to_subject_when_no_origin_names_a_file() {
        let file = bare_candidate(
            RuleId::Chk001,
            IssueSubject::File {
                path: "src/legacy.py".to_owned(),
            },
            "",
        );
        let import = bare_candidate(
            RuleId::Chk010,
            IssueSubject::Import {
                module: "requets".to_owned(),
                file: "src/app.py".to_owned(),
                line: 3,
                distribution: None,
            },
            "",
        );
        let manifest = DependencyOrigin {
            file: "scripts/tool.py".to_owned(),
            line: Some(4),
            label: "script dependencies[0]".to_owned(),
        };
        let mut script = bare_candidate(
            RuleId::Chk002,
            IssueSubject::ScriptDistribution {
                script: "scripts/tool.py".to_owned(),
                name: "rich".to_owned(),
            },
            "",
        );
        script.origins = vec![Origin::Manifest(manifest.clone())];

        let location = |file: Option<&str>, line: Option<u32>, manifest| IssueLocation {
            file: file.map(str::to_owned),
            line,
            manifest,
        };
        assert_eq!(
            location_from_candidate(&file),
            location(Some("src/legacy.py"), None, None)
        );
        assert_eq!(
            location_from_candidate(&import),
            location(Some("src/app.py"), Some(3), None)
        );
        assert_eq!(
            location_from_candidate(&script),
            location(Some("scripts/tool.py"), Some(4), Some(manifest))
        );
    }
}
