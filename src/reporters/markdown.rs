//! Markdown reporter for CI summaries.

use std::fmt::Write as _;

use crate::rules::{IssueReport, RuleId};

use super::format::{format_location_column, format_subject, group_title};
use super::traits::Reporter;
use super::types::RenderContext;

/// Markdown summary reporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownReporter;

impl Reporter for MarkdownReporter {
    fn render(&self, report: &IssueReport, context: &RenderContext) -> String {
        let mut out = String::new();
        let project = context.project_name.as_deref().unwrap_or("(unknown)");
        let _ = writeln!(out, "# yokei report — {project}\n");
        let _ = writeln!(out, "- Version: `{}`", context.version);
        let _ = writeln!(
            out,
            "- Mode: `{}` (production={})",
            context.mode.mode, context.production
        );
        let _ = writeln!(out, "- Issues: **{}**\n", report.summary.total);

        if report.issues.is_empty() {
            let _ = writeln!(out, "_No issues found._");
            return out;
        }

        let rules = [
            RuleId::Yok001,
            RuleId::Yok002,
            RuleId::Yok003,
            RuleId::Yok004,
            RuleId::Yok005,
            RuleId::Yok006,
            RuleId::Yok007,
            RuleId::Yok008,
            RuleId::Yok009,
            RuleId::Yok010,
        ];

        for rule in rules {
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
                let subject = format_subject(&issue.subject);
                let location = format_location_column(&issue.location);
                let message = issue.message.replace('|', "\\|");
                let _ = writeln!(
                    out,
                    "| {} | `{subject}` | `{location}` | {message} |",
                    issue.rule.as_code()
                );
            }
            let _ = writeln!(out);
        }

        out
    }
}
