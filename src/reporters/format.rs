//! Shared formatting helpers for reporters.

use crate::manifest::DependencyOrigin;
use crate::rules::{Issue, IssueLocation, IssueReport, IssueSubject, RuleId, SuppressReason};

/// Format a primary location column for human reporters.
pub(super) fn format_location_column(location: &IssueLocation) -> String {
    if let Some(origin) = &location.manifest {
        return format_manifest_origin(origin);
    }
    match (&location.file, location.line) {
        (Some(file), Some(line)) => format!("{file}:{line}"),
        (Some(file), None) => file.clone(),
        (None, _) => "-".to_owned(),
    }
}

fn format_manifest_origin(origin: &DependencyOrigin) -> String {
    match origin.line {
        Some(line) => format!("{}:{line}", origin.file),
        None => origin.file.clone(),
    }
}

/// Short subject label for compact output.
pub fn format_subject(subject: &IssueSubject) -> String {
    match subject {
        IssueSubject::File { path } => path.clone(),
        IssueSubject::Distribution { name } | IssueSubject::Binary { name } => name.clone(),
        IssueSubject::Symbol { module, name } => format!("{module}:{name}"),
        IssueSubject::Import { module, file, line } => format!("{file}:{line} {module}"),
    }
}

/// Short subject label annotated with workspace member metadata when present.
pub fn format_issue_subject(issue: &Issue) -> String {
    let subject = format_subject(&issue.subject);
    issue.workspace_member
        .as_ref()
        .map_or_else(|| subject.clone(), |member| format!("{member}:{subject}"))
}

/// Group title for the default reporter.
pub(super) fn group_title(rule: RuleId, count: usize) -> String {
    let label = match rule {
        RuleId::Chk001 => "Unused files",
        RuleId::Chk002 => "Unused dependencies",
        RuleId::Chk003 => "Missing dependencies",
        RuleId::Chk004 => "Transitive dependencies",
        RuleId::Chk005 => "Misplaced dependencies",
        RuleId::Chk006 => "Unused exports",
        RuleId::Chk007 => "Unused re-exports",
        RuleId::Chk008 => "Unlisted binaries",
        RuleId::Chk009 => "Duplicate dependencies",
        RuleId::Chk010 => "Unresolved imports",
    };
    format!("{label}  {count}")
}

/// Default reporter line: subject, location, message.
pub(super) fn format_issue_line(issue: &Issue) -> String {
    let subject = format_issue_subject(issue);
    let location = format_location_column(&issue.location);
    format!("  {subject:<24} {location:<22} {}", issue.message)
}

/// Severity label for reporters.
pub(super) fn severity_label(severity: crate::rules::Severity) -> &'static str {
    match severity {
        crate::rules::Severity::Error => "error",
        crate::rules::Severity::Warning => "warning",
        crate::rules::Severity::Info => "info",
    }
}

/// Count issues suppressed by the baseline file.
pub(super) fn baseline_suppressed_count(report: &IssueReport) -> usize {
    report
        .suppressed
        .iter()
        .filter(|suppressed| suppressed.reason == SuppressReason::Baseline)
        .count()
}

pub(super) fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", u32::from(ch));
            },
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Confidence;
    use crate::rules::{Issue, IssueLocation, RuleId, Severity};

    #[test]
    fn issue_subject_includes_workspace_member() {
        let issue = Issue {
            rule: RuleId::Chk003,
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: "missing".to_owned(),
            workspace_member: Some("api".to_owned()),
            location: IssueLocation {
                file: Some("services/api/src/api/main.py".to_owned()),
                line: Some(1),
                manifest: None,
            },
            subject: IssueSubject::Import {
                module: "requests".to_owned(),
                file: "services/api/src/api/main.py".to_owned(),
                line: 1,
            },
            explain: None,
        };

        assert_eq!(
            format_issue_subject(&issue),
            "api:services/api/src/api/main.py:1 requests"
        );
    }
}
