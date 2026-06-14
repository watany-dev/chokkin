//! Human-readable grouped reporter (§2 default output).

use std::fmt::Write as _;

use crate::config::ConfigSources;
use crate::rules::{IssueReport, RuleId};

use super::format::{format_issue_line, group_title};
use super::traits::Reporter;
use super::types::RenderContext;

/// Default human-readable reporter grouped by issue kind.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultReporter;

impl Reporter for DefaultReporter {
    fn render(&self, report: &IssueReport, context: &RenderContext) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "chokkin {}\n", context.version);

        let project = context.project_name.as_deref().unwrap_or("(unknown)");
        let _ = writeln!(out, "Project: {project}");
        if let Some(config) = &context.config_label {
            let _ = writeln!(out, "Config : {config}");
        }
        let _ = writeln!(
            out,
            "Mode   : {}, production={}\n",
            context.mode.mode, context.production
        );

        let rules = [
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
            let _ = writeln!(out, "{}", group_title(rule, issues.len()));
            for issue in issues {
                let _ = writeln!(out, "{}", format_issue_line(issue));
            }
            let _ = writeln!(out);
        }

        let _ = writeln!(out, "Summary: {} issues", report.summary.total);
        out
    }
}

/// Build a config source label from discovery output.
#[must_use]
pub fn config_label_from_sources(sources: &ConfigSources) -> String {
    let mut parts = Vec::new();
    if sources.dot_chokkin_toml.is_some() {
        parts.push(".chokkin.toml".to_owned());
    }
    if sources.chokkin_toml.is_some() {
        parts.push("chokkin.toml".to_owned());
    }
    if sources.pyproject_tool_chokkin {
        parts.push("pyproject.toml".to_owned());
    }
    if parts.is_empty() {
        if sources.used_defaults {
            "defaults".to_owned()
        } else {
            "(none)".to_owned()
        }
    } else {
        parts.join(", ")
    }
}
