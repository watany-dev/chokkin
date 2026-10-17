//! Human-readable `--fix` and baseline summaries written to stderr.

use std::fmt::Write;

use crate::baseline::BaselineReport;
use crate::fix::FixReport;

use super::format::{escape_control, format_subject};

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
            escape_control(&fix.file),
            escape_control(&fix.description)
        );
    }
    for skipped in &report.skipped {
        let _ = writeln!(
            out,
            "  skipped {} {} — {}: {}",
            skipped.rule.as_code(),
            format_subject(&skipped.subject),
            skipped.reason,
            escape_control(&skipped.detail)
        );
    }
    for reminder in &report.reminders {
        let _ = writeln!(out, "  reminder: {}", escape_control(reminder));
    }
    Some(out)
}

/// Render the baseline summary, or `None` when it neither wrote nor
/// suppressed anything.
#[must_use]
pub fn render_baseline_report(report: &BaselineReport) -> Option<String> {
    let path = escape_control(report.path.as_deref().unwrap_or("(unknown)"));
    if report.written > 0 {
        Some(format!(
            "Baseline: wrote {} issues to {path}\n",
            report.written
        ))
    } else if report.suppressed > 0 {
        Some(format!(
            "Baseline: suppressed {} existing issues from {path}\n",
            report.suppressed
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline(written: u32, suppressed: u32) -> BaselineReport {
        BaselineReport {
            path: Some("base\x1b[31m.json".to_owned()),
            suppressed,
            written,
        }
    }

    /// Writing wins over suppressing; neither prints nothing (#996).
    #[test]
    fn baseline_report_picks_the_message_by_count() {
        assert_eq!(render_baseline_report(&baseline(0, 0)), None);
        assert_eq!(
            render_baseline_report(&baseline(0, 2)).as_deref(),
            Some("Baseline: suppressed 2 existing issues from base\\u{1b}[31m.json\n")
        );
        assert_eq!(
            render_baseline_report(&baseline(3, 2)).as_deref(),
            Some("Baseline: wrote 3 issues to base\\u{1b}[31m.json\n")
        );
    }
}
