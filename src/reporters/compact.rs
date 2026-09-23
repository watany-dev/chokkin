//! One-line-per-issue reporter.

use std::fmt::Write as _;

use crate::rules::IssueReport;

use super::format::{
    baseline_suppressed_count, format_issue_subject, format_location_column, severity_label,
};
use super::types::RenderContext;

/// Compact reporter: one line per issue.
pub(super) fn render(report: &IssueReport, context: &RenderContext) -> String {
    let mut out = String::new();
    for issue in &report.issues {
        let _ = writeln!(
            out,
            "{} {} {} {} {}",
            issue.rule.as_code(),
            severity_label(issue.severity),
            issue.confidence.as_str(),
            format_issue_subject(issue),
            format_location_column(&issue.location),
        );
    }
    if report.issues.is_empty() {
        let _ = writeln!(
            out,
            "chokkin {} — no issues ({})",
            context.version, context.mode.mode
        );
    }
    let suppressed = baseline_suppressed_count(report);
    if suppressed > 0 {
        let _ = writeln!(out, "baseline suppressed {suppressed}");
    }
    out
}
