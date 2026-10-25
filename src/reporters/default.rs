//! Human-readable grouped reporter (§2 default output).

use std::fmt::Write as _;

use crate::config::ConfigSources;
use crate::rules::{IssueReport, RuleId};

use super::format::{
    baseline_suppressed_count, escape_control, format_issue_line, group_title, write_warning_count,
};
use super::types::RenderContext;

/// Default human-readable reporter grouped by issue kind.
pub(super) fn render(report: &IssueReport, context: &RenderContext) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "chokkin {}\n", context.version);

    let project = context.project_name.as_deref().unwrap_or("(unknown)");
    let _ = writeln!(out, "Project: {}", escape_control(project));
    if let Some(config) = &context.config_label {
        let _ = writeln!(out, "Config : {config}");
    }
    let _ = writeln!(
        out,
        "Mode   : {}, production={}\n",
        context.mode, context.production
    );

    for rule in RuleId::ALL {
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

    let suppressed = baseline_suppressed_count(report);
    if suppressed > 0 {
        let _ = writeln!(
            out,
            "Summary: {} issues ({} baseline-suppressed)",
            report.summary.total, suppressed
        );
    } else {
        let _ = writeln!(out, "Summary: {} issues", report.summary.total);
    }
    write_warning_count(&mut out, &context.diagnostics);
    out
}

/// Build a config source label from discovery output.
#[must_use]
pub fn config_label_from_sources(sources: &ConfigSources) -> String {
    config_label(sources, "pyproject.toml")
}

/// Shared by the analysis reporter and probe output, which label pyproject differently.
pub(crate) fn config_label(sources: &ConfigSources, pyproject_label: &str) -> String {
    let mut parts = Vec::new();
    if sources.dot_chokkin_toml.is_some() {
        parts.push(".chokkin.toml".to_owned());
    }
    if sources.chokkin_toml.is_some() {
        parts.push("chokkin.toml".to_owned());
    }
    if sources.pyproject_tool_chokkin {
        parts.push(pyproject_label.to_owned());
    }
    if parts.is_empty() {
        "defaults".to_owned()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// The `Config :` line names every file that contributed, in merge order
    /// (#1136).
    #[test]
    fn config_label_names_each_source_in_merge_order() {
        let mut sources = ConfigSources {
            dot_chokkin_toml: None,
            chokkin_toml: None,
            pyproject_tool_chokkin: false,
        };
        assert_eq!(config_label_from_sources(&sources), "defaults");
        sources.dot_chokkin_toml = Some(PathBuf::from(".chokkin.toml"));
        assert_eq!(config_label_from_sources(&sources), ".chokkin.toml");
        sources.chokkin_toml = Some(PathBuf::from("chokkin.toml"));
        assert_eq!(
            config_label_from_sources(&sources),
            ".chokkin.toml, chokkin.toml"
        );
        sources.pyproject_tool_chokkin = true;
        assert_eq!(
            config_label_from_sources(&sources),
            ".chokkin.toml, chokkin.toml, pyproject.toml"
        );
        assert_eq!(
            config_label(&sources, "pyproject.toml [tool.chokkin]"),
            ".chokkin.toml, chokkin.toml, pyproject.toml [tool.chokkin]"
        );
        sources.dot_chokkin_toml = None;
        assert_eq!(
            config_label_from_sources(&sources),
            "chokkin.toml, pyproject.toml"
        );
    }
}
