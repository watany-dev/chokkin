//! Minimal SARIF v2.1.0 reporter for GitHub code scanning (Phase 3 / v0.3).

use serde::Serialize;
use serde_json::{Value, json};

use crate::rules::metadata::default_rule_severity;
use crate::rules::{
    Issue, IssueReport, RuleId, Severity, issue_fingerprint, rule_help_text, rule_help_uri,
    rule_title,
};

use super::format::severity_label;
use super::traits::Reporter;
use super::types::RenderContext;

const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";

/// SARIF reporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct SarifReporter;

// Multi-key objects are structs, not `json!`: serde_json's `Value` sorts
// object keys, which would reorder the SARIF fields.
#[derive(Serialize)]
struct SarifLog {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: [SarifRun; 1],
}

#[derive(Serialize)]
struct SarifRun {
    tool: SarifTool,
    results: Vec<SarifResult>,
}

#[derive(Serialize)]
struct SarifTool {
    driver: SarifDriver,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifDriver {
    name: &'static str,
    semantic_version: &'static str,
    rules: Vec<SarifRule>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifRule {
    id: &'static str,
    short_description: Value,
    full_description: Value,
    help_uri: String,
    default_configuration: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifResult {
    rule_id: &'static str,
    level: &'static str,
    message: Value,
    partial_fingerprints: Value,
    properties: Value,
    locations: Vec<Value>,
}

impl Reporter for SarifReporter {
    fn render(&self, report: &IssueReport, context: &RenderContext) -> String {
        let log = SarifLog {
            schema: SARIF_SCHEMA,
            version: "2.1.0",
            runs: [SarifRun {
                tool: SarifTool {
                    driver: SarifDriver {
                        name: "chokkin",
                        semantic_version: context.version,
                        rules: all_rules().into_iter().map(sarif_rule).collect(),
                    },
                },
                results: report.issues.iter().map(sarif_result).collect(),
            }],
        };
        serde_json::to_string_pretty(&log).unwrap_or_default()
    }
}

fn sarif_rule(rule: RuleId) -> SarifRule {
    SarifRule {
        id: rule.as_code(),
        short_description: json!({ "text": rule_title(rule) }),
        full_description: json!({ "text": rule_help_text(rule) }),
        help_uri: rule_help_uri(rule),
        default_configuration: json!({ "level": sarif_level(default_rule_severity(rule)) }),
    }
}

fn sarif_result(issue: &Issue) -> SarifResult {
    SarifResult {
        rule_id: issue.rule.as_code(),
        level: sarif_level(issue.severity),
        message: json!({ "text": issue.message }),
        partial_fingerprints: json!({ "chokkin/v0": issue_fingerprint(issue) }),
        properties: json!({ "workspaceMember": issue.workspace_member }),
        locations: sarif_locations(issue),
    }
}

fn sarif_locations(issue: &Issue) -> Vec<Value> {
    let manifest = issue.location.manifest.as_ref();
    let file = issue
        .location
        .file
        .as_deref()
        .or_else(|| manifest.map(|origin| origin.file.as_str()));
    let line = issue
        .location
        .line
        .or_else(|| manifest.and_then(|origin| origin.line));
    file.map(|file| {
        json!({
            "physicalLocation": {
                "artifactLocation": { "uri": sarif_uri(file) },
                "region": { "startLine": line.unwrap_or(1) }
            }
        })
    })
    .into_iter()
    .collect()
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
