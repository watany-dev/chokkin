//! JSON reporter (v0.1 draft schema).

use std::fmt::Write as _;

use crate::rules::{Issue, IssueReport, IssueSubject};

use super::format::{baseline_suppressed_count, json_string};
use super::traits::Reporter;
use super::types::RenderContext;

/// JSON reporter for machine-readable output.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonReporter;

impl Reporter for JsonReporter {
    fn render(&self, report: &IssueReport, context: &RenderContext) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{{");
        let _ = writeln!(out, "  \"version\": {},", json_string(context.version));
        let _ = writeln!(
            out,
            "  \"project\": {},",
            json_string(context.project_name.as_deref().unwrap_or("(unknown)"))
        );
        let _ = writeln!(
            out,
            "  \"mode\": {},",
            json_string(context.mode.mode.as_str())
        );
        let _ = writeln!(
            out,
            "  \"production\": {},",
            if context.production { "true" } else { "false" }
        );
        let _ = writeln!(out, "  \"issues\": [");
        for (index, issue) in report.issues.iter().enumerate() {
            if index > 0 {
                let _ = writeln!(out, ",");
            }
            render_issue(&mut out, issue);
        }
        let _ = writeln!(out, "\n  ],");
        let _ = writeln!(out, "  \"summary\": {{");
        let _ = writeln!(out, "    \"total\": {},", report.summary.total);
        let _ = write!(out, "    \"by_code\": {{");
        let mut first = true;
        for (rule, count) in &report.summary.by_rule {
            if !first {
                let _ = write!(out, ",");
            }
            first = false;
            let _ = write!(out, "\n      {}: {count}", json_string(rule.as_code()));
        }
        if !report.summary.by_rule.is_empty() {
            let _ = writeln!(out);
        }
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out, "  }},");
        let _ = writeln!(out, "  \"suppressed\": {{");
        let _ = writeln!(
            out,
            "    \"baseline\": {}",
            baseline_suppressed_count(report)
        );
        let _ = writeln!(out, "  }}");
        let _ = write!(out, "}}");
        out
    }
}

fn render_issue(out: &mut String, issue: &Issue) {
    let _ = writeln!(out, "    {{");
    let _ = writeln!(
        out,
        "      \"code\": {},",
        json_string(issue.rule.as_code())
    );
    let _ = writeln!(
        out,
        "      \"severity\": {},",
        json_string(super::format::severity_label(issue.severity))
    );
    let _ = writeln!(
        out,
        "      \"confidence\": {},",
        json_string(issue.confidence.as_str())
    );
    let _ = writeln!(out, "      \"message\": {},", json_string(&issue.message));
    let _ = writeln!(
        out,
        "      \"workspace_member\": {},",
        optional_json_string(issue.workspace_member.as_deref())
    );
    let _ = writeln!(
        out,
        "      \"file\": {},",
        optional_json_path(issue.location.file.as_deref())
    );
    append_subject_fields(out, &issue.subject);
    let _ = write!(out, "      \"manifest\": ");
    if let Some(origin) = &issue.location.manifest {
        let _ = writeln!(out, "{{");
        let _ = writeln!(
            out,
            "        \"file\": {},",
            json_string(&normalize_path(&origin.file))
        );
        let _ = writeln!(
            out,
            "        \"line\": {}",
            origin
                .line
                .map_or_else(|| "null".to_owned(), |line| line.to_string())
        );
        let _ = write!(out, "      }}");
    } else {
        let _ = write!(out, "null");
    }
    let _ = writeln!(out);
    let _ = write!(out, "    }}");
}

fn optional_json_string(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), json_string)
}

fn optional_json_path(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), |path| json_string(&normalize_path(path)))
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn append_subject_fields(out: &mut String, subject: &IssueSubject) {
    match subject {
        IssueSubject::File { path } => {
            let _ = writeln!(out, "      \"path\": {},", json_string(&normalize_path(path)));
            let _ = writeln!(out, "      \"distribution\": null,");
            let _ = writeln!(out, "      \"symbol\": null,");
            let _ = writeln!(out, "      \"binary\": null,");
        },
        IssueSubject::Distribution { name } => {
            let _ = writeln!(out, "      \"path\": null,");
            let _ = writeln!(out, "      \"distribution\": {},", json_string(name));
            let _ = writeln!(out, "      \"symbol\": null,");
            let _ = writeln!(out, "      \"binary\": null,");
        },
        IssueSubject::Symbol { module, name } => {
            let _ = writeln!(out, "      \"path\": null,");
            let _ = writeln!(out, "      \"distribution\": null,");
            let _ = writeln!(
                out,
                "      \"symbol\": {},",
                json_string(&format!("{module}:{name}"))
            );
            let _ = writeln!(out, "      \"binary\": null,");
        },
        IssueSubject::Binary { name } => {
            let _ = writeln!(out, "      \"path\": null,");
            let _ = writeln!(out, "      \"distribution\": null,");
            let _ = writeln!(out, "      \"symbol\": null,");
            let _ = writeln!(out, "      \"binary\": {},", json_string(name));
        },
        IssueSubject::Import { module, file, line } => {
            let path = normalize_path(file);
            let _ = writeln!(out, "      \"path\": {},", json_string(&path));
            let _ = writeln!(out, "      \"distribution\": null,");
            let _ = writeln!(
                out,
                "      \"symbol\": {},",
                json_string(&format!("{path}:{line} {module}"))
            );
            let _ = writeln!(out, "      \"binary\": null,");
        },
    }
}
