//! JSON reporter (v0.3 stable schema).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::path_util::normalize_rel_path;
use crate::rules::{Issue, IssueReport, IssueSubject, issue_fingerprint, issue_stable_target};

use super::format::{baseline_suppressed_count, severity_label};
use super::types::RenderContext;

/// JSON reporter `schema_version` for the v0.3 stable contract.
const JSON_REPORT_SCHEMA_VERSION: &str = "1";

#[derive(Serialize)]
struct JsonReport<'a> {
    schema_version: &'static str,
    version: &'static str,
    project: &'a str,
    mode: &'static str,
    production: bool,
    issues: Vec<JsonIssue<'a>>,
    summary: JsonSummary,
    suppressed: JsonSuppressed,
    diagnostics: Vec<JsonDiagnostic<'a>>,
}

#[derive(Serialize)]
struct JsonDiagnostic<'a> {
    message: &'a str,
}

#[derive(Serialize)]
struct JsonSummary {
    total: u32,
    by_code: BTreeMap<&'static str, u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<JsonFiles>,
}

#[derive(Serialize)]
struct JsonFiles {
    runtime: usize,
    reachable_runtime: usize,
}

#[derive(Serialize)]
struct JsonSuppressed {
    baseline: usize,
}

#[derive(Serialize)]
struct JsonIssue<'a> {
    code: &'static str,
    severity: &'static str,
    confidence: &'static str,
    message: &'a str,
    fingerprint: String,
    target: String,
    workspace_member: Option<&'a str>,
    file: Option<String>,
    line: Option<u32>,
    path: Option<String>,
    distribution: Option<&'a str>,
    symbol: Option<String>,
    binary: Option<&'a str>,
    manifest: Option<JsonManifest>,
}

#[derive(Serialize)]
struct JsonManifest {
    file: String,
    line: Option<u32>,
}

/// JSON reporter for machine-readable output.
pub(super) fn render(report: &IssueReport, context: &RenderContext) -> String {
    let json = JsonReport {
        schema_version: JSON_REPORT_SCHEMA_VERSION,
        version: context.version,
        project: context.project_name.as_deref().unwrap_or("(unknown)"),
        mode: context.mode.as_str(),
        production: context.production,
        issues: report.issues.iter().map(json_issue).collect(),
        summary: JsonSummary {
            total: report.summary.total,
            by_code: report
                .summary
                .by_rule
                .iter()
                .map(|(rule, count)| (rule.as_code(), *count))
                .collect(),
            files: context.files.map(|files| JsonFiles {
                runtime: files.runtime,
                reachable_runtime: files.reachable_runtime,
            }),
        },
        suppressed: JsonSuppressed {
            baseline: baseline_suppressed_count(report),
        },
        diagnostics: context
            .diagnostics
            .iter()
            .map(|message| JsonDiagnostic { message })
            .collect(),
    };
    serde_json::to_string_pretty(&json).unwrap_or_default()
}

fn json_issue(issue: &Issue) -> JsonIssue<'_> {
    let (path, distribution, symbol, binary) = match &issue.subject {
        IssueSubject::File { path } => {
            (Some(normalize_rel_path(Path::new(path))), None, None, None)
        },
        IssueSubject::Distribution { name } => (None, Some(name.as_str()), None, None),
        IssueSubject::Symbol { module, name } => {
            (None, None, Some(format!("{module}:{name}")), None)
        },
        IssueSubject::Binary { name } => (None, None, None, Some(name.as_str())),
        IssueSubject::Import {
            module,
            file,
            distribution,
            ..
        } => (
            Some(normalize_rel_path(Path::new(file))),
            distribution.as_deref(),
            Some(module.clone()),
            None,
        ),
        IssueSubject::ScriptDistribution { script, name } => (
            Some(normalize_rel_path(Path::new(script))),
            Some(name.as_str()),
            None,
            None,
        ),
    };
    JsonIssue {
        code: issue.rule.as_code(),
        severity: severity_label(issue.severity),
        confidence: issue.confidence.as_str(),
        message: &issue.message,
        fingerprint: issue_fingerprint(issue),
        target: issue_stable_target(issue),
        workspace_member: issue.workspace_member.as_deref(),
        file: issue
            .location
            .file
            .as_deref()
            .map(|file| normalize_rel_path(Path::new(file))),
        line: issue.location.line,
        path,
        distribution,
        symbol,
        binary,
        manifest: issue.location.manifest.as_ref().map(|origin| JsonManifest {
            file: normalize_rel_path(Path::new(&origin.file)),
            line: origin.line,
        }),
    }
}
