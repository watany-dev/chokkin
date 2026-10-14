//! GitHub Actions annotation reporter (Phase 2 / v0.2).

use std::fmt::Write as _;
use std::path::Path;

use crate::path_util::normalize_rel_path;
use crate::rules::{Issue, IssueReport};

use super::format::{baseline_suppressed_count, condense_diagnostics, format_issue_subject};
use super::types::{Diagnostic, RenderContext};

/// GitHub Actions workflow-command reporter.
pub(super) fn render(report: &IssueReport, context: &RenderContext) -> String {
    let mut out = String::new();
    for issue in &report.issues {
        render_annotation(&mut out, issue);
    }
    // Annotations are the only channel a workflow run surfaces; stderr is
    // buried in the step log.
    for diagnostic in condense_diagnostics(&context.diagnostics) {
        render_diagnostic(&mut out, &diagnostic);
    }
    let suppressed = baseline_suppressed_count(report);
    if suppressed > 0 {
        let _ = writeln!(out, "chokkin: baseline suppressed {suppressed} issues");
    }
    out
}

fn render_annotation(out: &mut String, issue: &Issue) {
    let level = match issue.severity {
        crate::rules::Severity::Error => "error",
        crate::rules::Severity::Warning => "warning",
        crate::rules::Severity::Info => "notice",
    };
    let mut properties = Vec::new();
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
    if let Some(file) = file {
        properties.push(format!(
            "file={}",
            escape_property(&normalize_rel_path(Path::new(file)))
        ));
    }
    if let Some(line) = line {
        properties.push(format!("line={line}"));
    }
    properties.push(format!(
        "title={} {}",
        issue.rule.as_code(),
        escape_property(&format_issue_subject(issue))
    ));
    let _ = write!(out, "::{level} {}::", properties.join(","));
    let _ = writeln!(out, "{}", escape_message(&issue.message));
}

/// A warning that points into a file lands on it, so it shows on the PR
/// diff (#895).
fn render_diagnostic(out: &mut String, diagnostic: &Diagnostic) {
    let mut properties = Vec::new();
    if let Some(file) = &diagnostic.file {
        properties.push(format!(
            "file={}",
            escape_property(&normalize_rel_path(Path::new(file)))
        ));
    }
    if let Some(line) = diagnostic.line {
        properties.push(format!("line={line}"));
    }
    properties.push("title=chokkin".to_owned());
    let _ = writeln!(
        out,
        "::warning {}::{}",
        properties.join(","),
        escape_message(&diagnostic.text)
    );
}

fn escape_property(value: &str) -> String {
    escape_message(value)
        .replace(':', "%3A")
        .replace(',', "%2C")
}

fn escape_message(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}
