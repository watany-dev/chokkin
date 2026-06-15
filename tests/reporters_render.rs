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
    let mut by_rule = std::collections::BTreeMap::new();
    by_rule.insert(RuleId::Chk003, 1);
    let summary = IssueSummary { total: 1, by_rule };
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
fn github_reporter_normalizes_annotation_file_path() {
    let mut report = report();
    report.issues[0].location.file = Some("src\\acme\\app.py".to_owned());

    let rendered = render_issues(ReporterId::Github, &report, &context());

    assert!(rendered.contains("file=src/acme/app.py,line=7"));
    assert!(!rendered.contains("src\\acme\\app.py"));
}

#[test]
fn github_reporter_renders_info_as_notice() {
    let mut report = report();
    report.issues[0].severity = Severity::Info;

    let rendered = render_issues(ReporterId::Github, &report, &context());

    assert!(rendered.starts_with("::notice"));
}

#[test]
fn github_reporter_formats_annotation_without_location() {
    let mut report = report();
    report.issues[0].location = IssueLocation {
        file: None,
        line: None,
        manifest: None,
    };
    report.issues[0].subject = IssueSubject::Distribution {
        name: "requests".to_owned(),
    };

    let rendered = render_issues(ReporterId::Github, &report, &context());

    assert!(rendered.starts_with("::error title=CHK003 api%3Arequests::"));
    assert!(!rendered.starts_with("::error,"));
}

#[test]
fn sarif_reporter_renders_rule_location_workspace_and_schema() {
    let rendered = render_issues(ReporterId::Sarif, &report(), &context());
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid sarif json");
    assert!(rendered.contains("\"version\": \"2.1.0\""));
    assert!(rendered.contains("\"semanticVersion\": \"0.2.0-test\""));
    assert!(rendered.contains("\"id\": \"CHK003\""));
    assert!(rendered.contains("\"ruleId\": \"CHK003\""));
    assert!(rendered.contains("\"level\": \"error\""));
    assert!(rendered.contains("\"uri\": \"src/acme/app.py\""));
    assert!(rendered.contains("\"startLine\": 7"));
    assert!(rendered.contains("\"workspaceMember\": \"api\""));
    assert_eq!(parsed["version"], "2.1.0");
}

#[test]
fn json_reporter_renders_valid_json() {
    let rendered = render_issues(ReporterId::Json, &report(), &context());
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid json report");

    assert_eq!(parsed["version"], "0.2.0-test");
    assert_eq!(parsed["issues"][0]["code"], "CHK003");
    assert_eq!(
        parsed["issues"][0]["fingerprint"],
        "CHK003:api:src/acme/app.py:requests"
    );
    assert_eq!(
        parsed["issues"][0]["target"],
        "api:src/acme/app.py:requests"
    );
    assert_eq!(parsed["issues"][0]["workspace_member"], "api");
    assert_eq!(parsed["issues"][0]["line"], 7);
    assert_eq!(parsed["suppressed"]["baseline"], 1);
}

#[test]
fn json_reporter_normalizes_path_separators() {
    let mut report = report();
    report.issues[0].location.file = Some("src\\acme\\app.py".to_owned());
    report.issues[0].subject = IssueSubject::Import {
        module: "requests".to_owned(),
        file: "src\\acme\\app.py".to_owned(),
        line: 7,
    };

    let rendered = render_issues(ReporterId::Json, &report, &context());
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid json report");

    assert_eq!(parsed["issues"][0]["file"], "src/acme/app.py");
    assert_eq!(parsed["issues"][0]["path"], "src/acme/app.py");
    assert_eq!(parsed["issues"][0]["symbol"], "src/acme/app.py:7 requests");
}

#[test]
fn sarif_reporter_normalizes_artifact_uri_separators() {
    let mut report = report();
    report.issues[0].location.file = Some("src\\acme\\app.py".to_owned());

    let rendered = render_issues(ReporterId::Sarif, &report, &context());

    assert!(rendered.contains("\"uri\": \"src/acme/app.py\""));
    assert!(!rendered.contains("src\\\\acme\\\\app.py"));
}

#[test]
fn sarif_reporter_includes_stable_partial_fingerprint() {
    let mut report = report();
    report.issues[0].location.file = Some("src\\acme\\app.py".to_owned());
    report.issues[0].subject = IssueSubject::Import {
        module: "requests".to_owned(),
        file: "src\\acme\\app.py".to_owned(),
        line: 7,
    };

    let rendered = render_issues(ReporterId::Sarif, &report, &context());
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid sarif json");

    assert_eq!(
        parsed["runs"][0]["results"][0]["partialFingerprints"]["chokkin/v0"],
        "CHK003:api:src/acme/app.py:requests"
    );
}
