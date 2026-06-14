//! Reporter rendering regression tests for CI-facing formats.

#![allow(clippy::expect_used)]

use chokkin::{
    Confidence, ExitStatus, Issue, IssueLocation, IssueReport, IssueSubject, IssueSummary,
    ProjectMode, RenderContext, ReporterId, ResolveConfidence, ResolvedMode, RuleId, Severity,
    SuppressReason, SuppressedIssue, render_issues,
};

fn context() -> RenderContext {
    RenderContext {
        project_name: Some("demo".to_owned()),
        mode: ResolvedMode {
            mode: ProjectMode::App,
            confidence: ResolveConfidence::Certain,
        },
        production: false,
        version: "0.2.0-test",
        config_label: Some("pyproject.toml [tool.chokkin]".to_owned()),
    }
}

fn issue() -> Issue {
    Issue {
        rule: RuleId::Chk003,
        severity: Severity::Error,
        confidence: Confidence::Likely,
        message: "Missing dependency `requests`".to_owned(),
        workspace_member: Some("api".to_owned()),
        location: IssueLocation {
            file: Some("src/acme/app.py".to_owned()),
            line: Some(7),
            manifest: None,
        },
        subject: IssueSubject::Import {
            module: "requests".to_owned(),
            file: "src/acme/app.py".to_owned(),
            line: 7,
        },
        explain: None,
    }
}

fn report() -> IssueReport {
    let issue = issue();
    let mut summary = IssueSummary::default();
    summary.total = 1;
    summary.by_rule.insert(RuleId::Chk003, 1);
    IssueReport {
        issues: vec![issue.clone()],
        suppressed: vec![SuppressedIssue {
            issue,
            reason: SuppressReason::Baseline,
        }],
        summary,
        exit_status: ExitStatus::IssuesFound,
    }
}

#[test]
fn github_reporter_renders_annotation_and_baseline_summary() {
    let rendered = render_issues(ReporterId::Github, &report(), &context());
    assert!(rendered.contains("::error"));
    assert!(rendered.contains("file=src/acme/app.py,line=7"));
    assert!(rendered.contains("title=CHK003 api%3Asrc/acme/app.py%3A7 requests"));
    assert!(rendered.contains("Missing dependency `requests`"));
    assert!(rendered.contains("chokkin: baseline suppressed 1 issues"));
}

#[test]
fn sarif_reporter_renders_rule_location_workspace_and_schema() {
    let rendered = render_issues(ReporterId::Sarif, &report(), &context());
    assert!(rendered.contains("\"version\": \"2.1.0\""));
    assert!(rendered.contains("\"semanticVersion\": \"0.2.0-test\""));
    assert!(rendered.contains("\"id\": \"CHK003\""));
    assert!(rendered.contains("\"ruleId\": \"CHK003\""));
    assert!(rendered.contains("\"level\": \"error\""));
    assert!(rendered.contains("\"uri\": \"src/acme/app.py\""));
    assert!(rendered.contains("\"startLine\": 7"));
    assert!(rendered.contains("\"workspaceMember\": \"api\""));
}
