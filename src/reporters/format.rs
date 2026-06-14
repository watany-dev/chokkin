//! Shared formatting helpers for reporters.

use crate::manifest::DependencyOrigin;
use crate::rules::{Issue, IssueLocation, IssueSubject, RuleId};

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

/// Group title for the default reporter.
pub(super) fn group_title(rule: RuleId, count: usize) -> String {
    let label = match rule {
        RuleId::Yok001 => "Unused files",
        RuleId::Yok002 => "Unused dependencies",
        RuleId::Yok003 => "Missing dependencies",
        RuleId::Yok004 => "Transitive dependencies",
        RuleId::Yok005 => "Misplaced dependencies",
        RuleId::Yok006 => "Unused exports",
        RuleId::Yok007 => "Unused re-exports",
        RuleId::Yok008 => "Unlisted binaries",
        RuleId::Yok009 => "Duplicate dependencies",
        RuleId::Yok010 => "Unresolved imports",
    };
    format!("{label}  {count}")
}

/// Default reporter line: subject, location, message.
pub(super) fn format_issue_line(issue: &Issue) -> String {
    let subject = format_subject(&issue.subject);
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
