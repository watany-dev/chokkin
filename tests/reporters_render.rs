//! Reporter rendering regression tests for CI-facing formats.

#![allow(clippy::expect_used)]

use chokkin::{
    Confidence, DependencyOrigin, ExitStatus, Issue, IssueLocation, IssueReport, IssueSubject,
    IssueSummary, ProjectMode, RenderContext, ReporterId, RuleId, Severity, SuppressReason,
    SuppressedIssue, render_issues,
};

fn context() -> RenderContext {
    RenderContext {
        project_name: Some("demo".to_owned()),
        mode: ProjectMode::App,
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
            distribution: Some("requests".to_owned()),
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
    assert!(rendered.contains("\"helpUri\": \"https://github.com/watany-dev/chokkin/blob/main/docs/dev/spec.ja.md#chk003\""));
    assert_eq!(parsed["version"], "2.1.0");
    let chk003 = &parsed["runs"][0]["tool"]["driver"]["rules"][2];
    assert_eq!(chk003["id"], "CHK003");
    assert_eq!(chk003["shortDescription"]["text"], "missing dependency");
    assert!(
        chk003["fullDescription"]["text"]
            .as_str()
            .expect("fullDescription text")
            .starts_with("Source imports a distribution")
    );
}

#[test]
fn json_reporter_renders_valid_json() {
    let rendered = render_issues(ReporterId::Json, &report(), &context());
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid json report");

    assert_eq!(parsed["schema_version"], "1");
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
        distribution: Some("requests".to_owned()),
    };

    let rendered = render_issues(ReporterId::Json, &report, &context());
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid json report");

    assert_eq!(parsed["issues"][0]["file"], "src/acme/app.py");
    assert_eq!(parsed["issues"][0]["path"], "src/acme/app.py");
    assert_eq!(parsed["issues"][0]["line"], 7);
    assert_eq!(parsed["issues"][0]["symbol"], "requests");
    assert_eq!(parsed["issues"][0]["distribution"], "requests");
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
        distribution: Some("requests".to_owned()),
    };

    let rendered = render_issues(ReporterId::Sarif, &report, &context());
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid sarif json");

    assert_eq!(
        parsed["runs"][0]["results"][0]["partialFingerprints"]["chokkin/v0"],
        "CHK003:api:src/acme/app.py:requests"
    );
}

fn script_report() -> IssueReport {
    let mut report = report();
    report.issues[0].workspace_member = None;
    report.issues[0].location = IssueLocation {
        file: Some("scripts\\tool.py".to_owned()),
        line: Some(10),
        manifest: None,
    };
    report.issues[0].subject = IssueSubject::ScriptDistribution {
        script: "scripts\\tool.py".to_owned(),
        name: "pyyaml".to_owned(),
    };
    report
}

#[test]
fn reporters_render_pep723_script_subject() {
    let json = render_issues(ReporterId::Json, &script_report(), &context());
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json report");
    let issue = &parsed["issues"][0];
    assert_eq!(issue["target"], "script:scripts/tool.py:pyyaml");
    assert_eq!(issue["fingerprint"], "CHK003:script:scripts/tool.py:pyyaml");
    assert_eq!(issue["path"], "scripts/tool.py");
    assert_eq!(issue["distribution"], "pyyaml");
    assert_eq!(issue["file"], "scripts/tool.py");

    let sarif = render_issues(ReporterId::Sarif, &script_report(), &context());
    let parsed: serde_json::Value = serde_json::from_str(&sarif).expect("valid sarif json");
    assert!(sarif.contains("\"uri\": \"scripts/tool.py\""));
    assert!(sarif.contains("\"startLine\": 10"));
    assert_eq!(
        parsed["runs"][0]["results"][0]["partialFingerprints"]["chokkin/v0"],
        "CHK003:script:scripts/tool.py:pyyaml"
    );

    let github = render_issues(ReporterId::Github, &script_report(), &context());
    assert!(github.contains("title=CHK003 script%3A"));
}

fn empty_report() -> IssueReport {
    IssueReport {
        issues: Vec::new(),
        suppressed: Vec::new(),
        summary: IssueSummary {
            total: 0,
            by_rule: std::collections::BTreeMap::new(),
        },
        exit_status: ExitStatus::Success,
    }
}

fn unused_dependency(
    name: &str,
    severity: Severity,
    manifest: DependencyOrigin,
    message: &str,
) -> Issue {
    Issue {
        rule: RuleId::Chk002,
        severity,
        confidence: Confidence::Certain,
        message: message.to_owned(),
        workspace_member: None,
        location: IssueLocation {
            file: None,
            line: None,
            manifest: Some(manifest),
        },
        subject: IssueSubject::Distribution {
            name: name.to_owned(),
        },
        explain: None,
    }
}

/// Issues are out of rule order so grouped reporters must reorder while compact keeps input order.
fn mixed_report() -> IssueReport {
    let mut report = report();
    report.issues.push(unused_dependency(
        "boto3",
        Severity::Error,
        DependencyOrigin {
            file: "pyproject.toml".to_owned(),
            line: Some(18),
            label: "project.dependencies[0]".to_owned(),
        },
        "Unused dependency `boto3`",
    ));
    report.issues.push(unused_dependency(
        "python-dotenv",
        Severity::Warning,
        DependencyOrigin {
            file: "requirements.txt".to_owned(),
            line: None,
            label: "requirements.txt".to_owned(),
        },
        "Unused dependency `python-dotenv` | dev only",
    ));
    report.summary.total = 3;
    report.summary.by_rule.insert(RuleId::Chk002, 2);
    report
}

#[test]
fn default_reporter_renders_empty_report_exactly() {
    let rendered = render_issues(ReporterId::Default, &empty_report(), &context());
    assert_eq!(
        rendered,
        concat!(
            "chokkin 0.2.0-test\n",
            "\n",
            "Project: demo\n",
            "Config : pyproject.toml [tool.chokkin]\n",
            "Mode   : app, production=false\n",
            "\n",
            "Summary: 0 issues\n",
        )
    );
}

#[test]
fn default_reporter_groups_mixed_rules_in_rule_order_exactly() {
    let rendered = render_issues(ReporterId::Default, &mixed_report(), &context());
    assert_eq!(
        rendered,
        concat!(
            "chokkin 0.2.0-test\n",
            "\n",
            "Project: demo\n",
            "Config : pyproject.toml [tool.chokkin]\n",
            "Mode   : app, production=false\n",
            "\n",
            "Unused dependencies  2\n",
            "  boto3                    pyproject.toml:18      Unused dependency `boto3`\n",
            "  python-dotenv            requirements.txt       Unused dependency `python-dotenv` | dev only\n",
            "\n",
            "Missing dependencies  1\n",
            "  api:src/acme/app.py:7 requests src/acme/app.py:7      Missing dependency `requests`\n",
            "\n",
            "Summary: 3 issues (1 baseline-suppressed)\n",
        )
    );
}

#[test]
fn compact_reporter_renders_empty_report_exactly() {
    let rendered = render_issues(ReporterId::Compact, &empty_report(), &context());
    assert_eq!(rendered, "chokkin 0.2.0-test \u{2014} no issues (app)\n");
}

#[test]
fn compact_reporter_renders_one_line_per_issue_in_input_order_exactly() {
    let rendered = render_issues(ReporterId::Compact, &mixed_report(), &context());
    assert_eq!(
        rendered,
        concat!(
            "CHK003 error likely api:src/acme/app.py:7 requests src/acme/app.py:7\n",
            "CHK002 error certain boto3 pyproject.toml:18\n",
            "CHK002 warning certain python-dotenv requirements.txt\n",
            "baseline suppressed 1\n",
        )
    );
}

#[test]
fn markdown_reporter_renders_empty_report_exactly() {
    let rendered = render_issues(ReporterId::Markdown, &empty_report(), &context());
    assert_eq!(
        rendered,
        concat!(
            "# chokkin report \u{2014} demo\n",
            "\n",
            "- Version: `0.2.0-test`\n",
            "- Mode: `app` (production=false)\n",
            "- Issues: **0**\n",
            "\n",
            "_No issues found._\n",
        )
    );
}

#[test]
fn markdown_reporter_renders_rule_tables_and_escapes_pipes_exactly() {
    let rendered = render_issues(ReporterId::Markdown, &mixed_report(), &context());
    assert_eq!(
        rendered,
        concat!(
            "# chokkin report \u{2014} demo\n",
            "\n",
            "- Version: `0.2.0-test`\n",
            "- Mode: `app` (production=false)\n",
            "- Issues: **3**\n",
            "- Baseline suppressed: **1**\n",
            "\n",
            "## Unused dependencies  2\n",
            "\n",
            "| Code | Subject | Location | Message |\n",
            "| --- | --- | --- | --- |\n",
            "| CHK002 | `boto3` | `pyproject.toml:18` | Unused dependency `boto3` |\n",
            "| CHK002 | `python-dotenv` | `requirements.txt` | Unused dependency `python-dotenv` \\| dev only |\n",
            "\n",
            "## Missing dependencies  1\n",
            "\n",
            "| Code | Subject | Location | Message |\n",
            "| --- | --- | --- | --- |\n",
            "| CHK003 | `api:src/acme/app.py:7 requests` | `src/acme/app.py:7` | Missing dependency `requests` |\n",
            "\n",
        )
    );
}
