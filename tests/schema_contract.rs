//! v0.3 contract stabilization regression tests.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use chokkin::{
    Confidence, ExitStatus, Issue, IssueLocation, IssueReport, IssueSubject, IssueSummary,
    ProjectMode, RenderContext, ReporterId, ResolveConfidence, ResolvedMode, RuleId, Severity,
    apply_baseline, render_issues, write_baseline,
};
use jsonschema::Validator;
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

fn validator_for(name: &str) -> Validator {
    Validator::new(&load_schema(name)).expect("compile schema")
}

#[test]
fn json_report_validates_against_published_schema() {
    let rendered = render_issues(ReporterId::Json, &sample_report(), &context());
    let report: Value = serde_json::from_str(&rendered).expect("valid json report");
    let validator = validator_for("chokkin-report.schema.json");

    validator
        .validate(&report)
        .expect("json report should match published schema");
    assert_eq!(report["schema_version"], "1");
}

#[test]
fn json_schema_rejects_invalid_rule_code_prefix() {
    let validator = validator_for("chokkin-report.schema.json");
    let mut report: Value = serde_json::from_str(&render_issues(
        ReporterId::Json,
        &sample_report(),
        &context(),
    ))
    .expect("valid json report");
    report["issues"][0]["code"] = Value::String("CHK002EXTRA".to_owned());

    assert!(
        validator.validate(&report).is_err(),
        "invalid rule code prefix should fail schema validation"
    );
}

#[test]
fn baseline_v03_validates_and_reads_v02_without_schema_version() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let baseline = dir.path().join("chokkin-baseline.json");

    write_baseline(&sample_report(), dir.path(), &baseline).expect("write baseline");
    let written = fs::read_to_string(&baseline).expect("read baseline");
    let parsed: Value = serde_json::from_str(&written).expect("parse baseline");
    assert_eq!(parsed["schema_version"], "1");
    validator_for("chokkin-baseline.schema.json")
        .validate(&parsed)
        .expect("v0.3 baseline should match published schema");

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
