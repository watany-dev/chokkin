//! v0.3 contract stabilization regression tests.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use chokkin::{
    Confidence, ExitStatus, Issue, IssueLocation, IssueReport, IssueSubject, IssueSummary,
    ProjectMode, RenderContext, ReporterId, ResolveConfidence, ResolvedMode, RuleId, Severity,
    apply_baseline, render_issues, write_baseline,
};
use serde_json::Value;

fn context() -> RenderContext {
    RenderContext {
        project_name: Some("demo".to_owned()),
        mode: ResolvedMode {
            mode: ProjectMode::App,
            confidence: ResolveConfidence::Certain,
        },
        production: false,
        version: "0.3.0-test",
        config_label: Some("pyproject.toml [tool.chokkin]".to_owned()),
    }
}

fn sample_issue() -> Issue {
    Issue {
        rule: RuleId::Chk006,
        severity: Severity::Warning,
        confidence: Confidence::Certain,
        message: "unused export".to_owned(),
        workspace_member: None,
        location: IssueLocation {
            file: Some("src/acme/api.py".to_owned()),
            line: Some(12),
            manifest: None,
        },
        subject: IssueSubject::Symbol {
            module: "acme.api".to_owned(),
            name: "public_api".to_owned(),
        },
        explain: None,
    }
}

fn sample_report() -> IssueReport {
    IssueReport {
        issues: vec![sample_issue()],
        suppressed: Vec::new(),
        summary: IssueSummary {
            total: 1,
            by_rule: std::iter::once((RuleId::Chk006, 1)).collect(),
        },
        exit_status: ExitStatus::IssuesFound,
    }
}

fn load_schema(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs/schema")
        .join(name);
    let contents = fs::read_to_string(path).expect("read schema");
    serde_json::from_str(&contents).expect("parse schema")
}

#[test]
fn json_report_matches_published_schema_shape() {
    let rendered = render_issues(ReporterId::Json, &sample_report(), &context());
    let report: Value = serde_json::from_str(&rendered).expect("valid json report");
    let schema = load_schema("chokkin-report.schema.json");

    assert_required_fields(&report, schema["required"].as_array().expect("required"));
    assert_eq!(report["schema_version"], "1");

    let issue = &report["issues"][0];
    let issue_schema = &schema["$defs"]["issue"];
    assert_required_fields(
        issue,
        issue_schema["required"].as_array().expect("issue required"),
    );
}

#[test]
fn baseline_v03_writes_schema_version_and_reads_v02_without_it() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let baseline = dir.path().join("chokkin-baseline.json");

    write_baseline(&sample_report(), dir.path(), &baseline).expect("write baseline");
    let written = fs::read_to_string(&baseline).expect("read baseline");
    let parsed: Value = serde_json::from_str(&written).expect("parse baseline");
    assert_eq!(parsed["schema_version"], "1");

    let v02 = r#"{
  "chokkin_version": "0.2.0",
  "generated_at": "unix:1",
  "issues": [
    {
      "fingerprint": "CHK006:src/acme/api.py:public_api",
      "code": "CHK006",
      "target": "src/acme/api.py:public_api"
    }
  ]
}"#;
    fs::write(&baseline, format!("{v02}\n")).expect("write v0.2 baseline");

    let mut report = sample_report();
    apply_baseline(&mut report, dir.path(), &baseline).expect("apply v0.2 baseline");
    assert!(report.issues.is_empty());
    assert_eq!(report.exit_status, ExitStatus::Success);
}

fn assert_required_fields(value: &Value, required: &[Value]) {
    for field in required {
        let key = field.as_str().expect("field name");
        assert!(
            value.get(key).is_some(),
            "missing required field `{key}` in {value}"
        );
    }
}
