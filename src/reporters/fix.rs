//! Human-readable `--fix` summary written to stderr.

use std::fmt::Write;

use crate::fix::FixReport;

use super::format::format_subject;

/// Render the fix summary, or `None` when no fix was considered.
#[must_use]
pub fn render_fix_report(report: &FixReport) -> Option<String> {
    if report.applied.is_empty() && report.skipped.is_empty() && report.reminders.is_empty() {
        return None;
    }
    let mut out = String::from("Fixes:\n");
    let verb = if report.dry_run { "planned" } else { "applied" };
    for fix in &report.applied {
        let _ = writeln!(
            out,
            "  {verb} {} {} in {} — {}",
            fix.rule.as_code(),
            format_subject(&fix.subject),
            fix.file,
            fix.description
        );
    }
    for skipped in &report.skipped {
        let _ = writeln!(
            out,
            "  skipped {} {} — {}: {}",
            skipped.rule.as_code(),
            format_subject(&skipped.subject),
            skipped.reason,
            skipped.detail
        );
    }
    for reminder in &report.reminders {
        let _ = writeln!(out, "  reminder: {reminder}");
    }
    Some(out)
}
