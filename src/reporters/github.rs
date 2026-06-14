//! GitHub Actions annotation reporter (Phase 2 / v0.2).

use std::fmt::Write as _;

use crate::rules::{Issue, IssueReport};

use super::format::{baseline_suppressed_count, format_subject};
use super::traits::Reporter;
use super::types::RenderContext;

/// GitHub Actions workflow-command reporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct GithubReporter;

impl Reporter for GithubReporter {
    fn render(&self, report: &IssueReport, _context: &RenderContext) -> String {
        let mut out = String::new();
        for issue in &report.issues {
            render_annotation(&mut out, issue);
        }
        let suppressed = baseline_suppressed_count(report);
        if suppressed > 0 {
            let _ = writeln!(out, "chokkin: baseline suppressed {suppressed} issues");
        }
        out
    }
}

fn render_annotation(out: &mut String, issue: &Issue) {
    let level = match issue.severity {
        crate::rules::Severity::Error => "error",
        crate::rules::Severity::Warning | crate::rules::Severity::Info => "warning",
    };
    let _ = write!(out, "::{level}");
    let file = issue
        .location
        .file
        .as_deref()
        .or(issue.location.manifest.as_ref().map(|origin| origin.file.as_str()));
    let line = issue.location.line.or_else(|| {
        issue
            .location
            .manifest
            .as_ref()
            .and_then(|origin| origin.line)
    });
    if let Some(file) = file {
        let _ = write!(out, " file={}", escape_property(file));
    }
    if let Some(line) = line {
        let _ = write!(out, ",line={line}");
    }
    let _ = write!(
        out,
        ",title={} {}::",
        issue.rule.as_code(),
        escape_property(&format_subject(&issue.subject))
    );
    let _ = writeln!(out, "{}", escape_message(&issue.message));
}

fn escape_property(value: &str) -> String {
    escape_message(value).replace(':', "%3A").replace(',', "%2C")
}

fn escape_message(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}
