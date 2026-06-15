//! Minimal SARIF v2.1.0 reporter for GitHub code scanning (Phase 2 / v0.2).

use std::fmt::Write as _;

use crate::rules::{Issue, IssueReport, RuleId, Severity, issue_fingerprint};

use super::format::{json_string, severity_label};
use super::traits::Reporter;
use super::types::RenderContext;

const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";

/// SARIF reporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct SarifReporter;

impl Reporter for SarifReporter {
    fn render(&self, report: &IssueReport, context: &RenderContext) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{{");
        let _ = writeln!(out, "  \"$schema\": {},", json_string(SARIF_SCHEMA));
        let _ = writeln!(out, "  \"version\": \"2.1.0\",");
        let _ = writeln!(out, "  \"runs\": [");
        let _ = writeln!(out, "    {{");
        render_tool(&mut out, context);
        let _ = writeln!(out, ",");
        render_results(&mut out, &report.issues);
        let _ = writeln!(out);
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out, "  ]");
        let _ = write!(out, "}}");
        out
    }
}

fn render_tool(out: &mut String, context: &RenderContext) {
    let _ = writeln!(out, "      \"tool\": {{");
    let _ = writeln!(out, "        \"driver\": {{");
    let _ = writeln!(out, "          \"name\": \"chokkin\",");
    let _ = writeln!(
        out,
        "          \"semanticVersion\": {},",
        json_string(context.version)
    );
    let _ = writeln!(out, "          \"rules\": [");
    for (index, rule) in all_rules().iter().enumerate() {
        if index > 0 {
            let _ = writeln!(out, ",");
        }
        render_rule(out, *rule);
    }
    let _ = writeln!(out, "\n          ]");
    let _ = writeln!(out, "        }}");
    let _ = write!(out, "      }}");
}

fn render_rule(out: &mut String, rule: RuleId) {
    let _ = writeln!(out, "            {{");
    let _ = writeln!(
        out,
        "              \"id\": {},",
        json_string(rule.as_code())
    );
    let _ = writeln!(
        out,
        "              \"shortDescription\": {{ \"text\": {} }},",
        json_string(rule_title(rule))
    );
    let _ = writeln!(
        out,
        "              \"defaultConfiguration\": {{ \"level\": {} }}",
        json_string(sarif_level(rule_default_severity(rule)))
    );
    let _ = write!(out, "            }}");
}

fn render_results(out: &mut String, issues: &[Issue]) {
    let _ = writeln!(out, "      \"results\": [");
    for (index, issue) in issues.iter().enumerate() {
        if index > 0 {
            let _ = writeln!(out, ",");
        }
        render_result(out, issue);
    }
    let _ = writeln!(out, "\n      ]");
}

fn render_result(out: &mut String, issue: &Issue) {
    let _ = writeln!(out, "        {{");
    let _ = writeln!(
        out,
        "          \"ruleId\": {},",
        json_string(issue.rule.as_code())
    );
    let _ = writeln!(
        out,
        "          \"level\": {},",
        json_string(sarif_level(issue.severity))
    );
    let _ = writeln!(
        out,
        "          \"message\": {{ \"text\": {} }},",
        json_string(&issue.message)
    );
    let _ = writeln!(out, "          \"partialFingerprints\": {{");
    let _ = writeln!(
        out,
        "            \"chokkin/v0\": {}",
        json_string(&issue_fingerprint(issue))
    );
    let _ = writeln!(out, "          }},");
    let _ = writeln!(out, "          \"properties\": {{");
    let _ = writeln!(
        out,
        "            \"workspaceMember\": {}",
        issue
            .workspace_member
            .as_deref()
            .map_or_else(|| "null".to_owned(), json_string)
    );
    let _ = writeln!(out, "          }},");
    render_locations(out, issue);
    let _ = write!(out, "        }}");
}

fn render_locations(out: &mut String, issue: &Issue) {
    let file = issue.location.file.as_deref().or_else(|| {
        issue
            .location
            .manifest
            .as_ref()
            .map(|origin| origin.file.as_str())
    });
    let line = issue.location.line.or_else(|| {
        issue
            .location
            .manifest
            .as_ref()
            .and_then(|origin| origin.line)
    });
    let Some(file) = file else {
        let _ = writeln!(out, "          \"locations\": []");
        return;
    };
    let _ = writeln!(out, "          \"locations\": [");
    let _ = writeln!(out, "            {{");
    let _ = writeln!(out, "              \"physicalLocation\": {{");
    let _ = writeln!(out, "                \"artifactLocation\": {{");
    let _ = writeln!(
        out,
        "                  \"uri\": {}",
        json_string(&sarif_uri(file))
    );
    let _ = writeln!(out, "                }},");
    let _ = writeln!(out, "                \"region\": {{");
    let _ = writeln!(
        out,
        "                  \"startLine\": {}",
        line.unwrap_or(1)
    );
    let _ = writeln!(out, "                }}");
    let _ = writeln!(out, "              }}");
    let _ = writeln!(out, "            }}");
    let _ = writeln!(out, "          ]");
}

fn all_rules() -> [RuleId; 10] {
    [
        RuleId::Chk001,
        RuleId::Chk002,
        RuleId::Chk003,
        RuleId::Chk004,
        RuleId::Chk005,
        RuleId::Chk006,
        RuleId::Chk007,
        RuleId::Chk008,
        RuleId::Chk009,
        RuleId::Chk010,
    ]
}

fn rule_title(rule: RuleId) -> &'static str {
    match rule {
        RuleId::Chk001 => "unused file",
        RuleId::Chk002 => "unused dependency",
        RuleId::Chk003 => "missing dependency",
        RuleId::Chk004 => "transitive dependency",
        RuleId::Chk005 => "misplaced dependency",
        RuleId::Chk006 => "unused export",
        RuleId::Chk007 => "unused re-export",
        RuleId::Chk008 => "unlisted binary",
        RuleId::Chk009 => "duplicate dependency",
        RuleId::Chk010 => "unresolved import",
    }
}

fn rule_default_severity(rule: RuleId) -> Severity {
    match rule {
        RuleId::Chk002 | RuleId::Chk003 | RuleId::Chk004 => Severity::Error,
        RuleId::Chk001
        | RuleId::Chk005
        | RuleId::Chk006
        | RuleId::Chk007
        | RuleId::Chk008
        | RuleId::Chk009
        | RuleId::Chk010 => Severity::Warning,
    }
}

fn sarif_level(severity: Severity) -> &'static str {
    match severity_label(severity) {
        "error" => "error",
        "info" => "note",
        _ => "warning",
    }
}

fn sarif_uri(path: &str) -> String {
    path.replace('\\', "/")
}
