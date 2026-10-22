//! Markdown reporter for CI summaries.

use std::fmt::Write as _;

use crate::rules::{IssueReport, RuleId};

use super::format::{
    baseline_suppressed_count, condense_diagnostics, escape_control, format_issue_subject,
    format_location_column, group_title,
};
use super::types::{Diagnostic, RenderContext};

/// Markdown summary reporter.
pub(super) fn render(report: &IssueReport, context: &RenderContext) -> String {
    let mut out = String::new();
    let project = context.project_name.as_deref().unwrap_or("(unknown)");
    let _ = writeln!(out, "# chokkin report — {}\n", escape_control(project));
    let _ = writeln!(out, "- Version: `{}`", context.version);
    let _ = writeln!(
        out,
        "- Mode: `{}` (production={})",
        context.mode, context.production
    );
    let _ = writeln!(out, "- Issues: **{}**", report.summary.total);
    let suppressed = baseline_suppressed_count(report);
    if suppressed > 0 {
        let _ = writeln!(out, "- Baseline suppressed: **{suppressed}**");
    }
    if !context.diagnostics.is_empty() {
        let _ = writeln!(out, "- Warnings: **{}**", context.diagnostics.len());
    }
    let _ = writeln!(out);

    if report.issues.is_empty() {
        let _ = writeln!(out, "_No issues found._");
        if !context.diagnostics.is_empty() {
            let _ = writeln!(out);
        }
    }

    for rule in RuleId::ALL {
        let issues: Vec<_> = report
            .issues
            .iter()
            .filter(|issue| issue.rule == rule)
            .collect();
        if issues.is_empty() {
            continue;
        }
        let _ = writeln!(out, "## {}\n", group_title(rule, issues.len()));
        let _ = writeln!(out, "| Code | Subject | Location | Message |");
        let _ = writeln!(out, "| --- | --- | --- | --- |");
        for issue in issues {
            let subject = code_span(&format_issue_subject(issue));
            let location = code_span(&format_location_column(&issue.location));
            let message = escape_control(&issue.message).replace('|', "\\|");
            let _ = writeln!(
                out,
                "| {} | {subject} | {location} | {message} |",
                issue.rule.as_code()
            );
        }
        let _ = writeln!(out);
    }

    write_diagnostics(&mut out, &context.diagnostics);
    out
}

/// `text` as one code span in a table cell: `|` escaped so the cell does not
/// split, a fence longer than any backtick run inside, and a space inside the
/// fence when the text would otherwise merge with it or lose its own spaces.
/// A backslash is left alone: GFM reads `\|` in a cell as an escaped pipe
/// whatever precedes it.
fn code_span(text: &str) -> String {
    if text.is_empty() {
        // An empty span is no span; a lone space shows as itself.
        return "` `".to_owned();
    }
    let text = text.replace('|', "\\|");
    let longest = text
        .split(|ch| ch != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest + 1);
    let pad = text.starts_with('`')
        || text.ends_with('`')
        || (text.starts_with(' ') && text.ends_with(' '));
    let pad = if pad { " " } else { "" };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// The step summary is often the only output a CI reader opens, so the
/// warnings are listed rather than counted, repeated kinds condensed.
fn write_diagnostics(out: &mut String, diagnostics: &[Diagnostic]) {
    if diagnostics.is_empty() {
        return;
    }
    let _ = writeln!(out, "## Warnings  {}\n", diagnostics.len());
    for diagnostic in condense_diagnostics(diagnostics) {
        let text = diagnostic.text.replace(['\r', '\n'], " ");
        let _ = writeln!(out, "- {}", escape_control(&text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_spans_outlast_backticks_and_escape_pipes() {
        assert_eq!(code_span("boto3"), "`boto3`");
        assert_eq!(code_span("a|b"), "`a\\|b`");
        assert_eq!(code_span("a`b"), "``a`b``");
        assert_eq!(code_span("`a"), "`` `a ``");
        assert_eq!(code_span(""), "` `");
    }

    /// Code spans (#1105): what a GFM table and `CommonMark` read back from a
    /// span is the text it was made from, on one line.
    mod props {
        use super::*;
        use proptest::prelude::*;

        /// The text a reader gets from `span`: the fence and one padding
        /// space on each side dropped, then each `\|` read as `|`.
        fn read_back(span: &str) -> Option<String> {
            let fence = span.len() - span.trim_start_matches('`').len();
            let inner = span.get(fence..span.len().checked_sub(fence)?)?;
            if fence == 0 || !span.ends_with(&"`".repeat(fence)) {
                return None;
            }
            if inner.split(|ch| ch != '`').any(|run| run.len() == fence) {
                return None;
            }
            let inner = if inner.len() >= 2
                && inner.starts_with(' ')
                && inner.ends_with(' ')
                && !inner.chars().all(|ch| ch == ' ')
            {
                &inner[1..inner.len() - 1]
            } else {
                inner
            };
            Some(inner.replace("\\|", "|"))
        }

        proptest! {
            #[test]
            fn a_code_span_reads_back_as_its_text(text in "[a-z |`\\\\:./]{1,12}") {
                // Text that is all spaces is the one case a span keeps
                // unstripped, padding included; names never are.
                prop_assume!(!text.chars().all(|ch| ch == ' '));
                let span = code_span(&text);
                prop_assert!(!span.contains('\n'));
                prop_assert_eq!(read_back(&span), Some(text));
            }
        }
    }
}
