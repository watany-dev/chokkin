//! Shared formatting helpers for reporters.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::manifest::DependencyOrigin;
use crate::rules::{
    Issue, IssueLocation, IssueReport, IssueSubject, RuleId, SuppressReason, import_site,
};

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
pub(super) fn format_subject(subject: &IssueSubject) -> String {
    match subject {
        IssueSubject::File { path } => path.clone(),
        IssueSubject::Distribution { name } | IssueSubject::Binary { name } => name.clone(),
        IssueSubject::Symbol { module, name } => format!("{module}:{name}"),
        IssueSubject::Import {
            module, file, line, ..
        } => format!("{} {module}", import_site(file, *line)),
        IssueSubject::ScriptDistribution { script, name } => format!("script:{script}:{name}"),
    }
}

/// Short subject label annotated with workspace member metadata when present.
pub(super) fn format_issue_subject(issue: &Issue) -> String {
    let subject = format_subject(&issue.subject);
    issue
        .workspace_member
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

/// Point text reporters at stderr, which carries the full warning list.
pub(super) fn write_warning_count(out: &mut String, diagnostics: &[String]) {
    if !diagnostics.is_empty() {
        let _ = writeln!(out, "Warnings: {} (see stderr)", diagnostics.len());
    }
}

/// Lines of one warning kind shown before the rest are only counted.
const REPEATS_SHOWN: usize = 5;

/// `diagnostics` with each kind cut to its first [`REPEATS_SHOWN`] lines and
/// a count of the rest, for outputs a person reads top to bottom (#888).
pub(crate) fn condense_diagnostics(diagnostics: &[String]) -> Vec<String> {
    let mut totals = HashMap::<String, usize>::new();
    for diagnostic in diagnostics {
        *totals.entry(diagnostic_kind(diagnostic)).or_default() += 1;
    }
    let mut shown = HashMap::<String, usize>::new();
    let mut out = Vec::new();
    for diagnostic in diagnostics {
        let kind = diagnostic_kind(diagnostic);
        let seen = shown.entry(kind.clone()).or_default();
        *seen += 1;
        if *seen <= REPEATS_SHOWN {
            out.push(diagnostic.clone());
        }
        let total = totals[&kind];
        if *seen == REPEATS_SHOWN && total > REPEATS_SHOWN {
            let area = diagnostic.split_once(':').map_or("", |(area, _)| area);
            out.push(format!(
                "{area}: … and {} more like the above (`--reporter json` lists every warning)",
                total - REPEATS_SHOWN
            ));
        }
    }
    out
}

/// A warning with its quoted paths and names and its numbers blanked out, so
/// warnings that differ only in where they point share a kind.
fn diagnostic_kind(diagnostic: &str) -> String {
    let unquoted = diagnostic
        .split('`')
        .step_by(2)
        .collect::<Vec<_>>()
        .join("``");
    let mut kind = String::with_capacity(unquoted.len());
    for ch in unquoted.chars() {
        if !ch.is_ascii_digit() {
            kind.push(ch);
        } else if !kind.ends_with('#') {
            kind.push('#');
        }
    }
    kind
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
                line: Some(1),
                distribution: Some("requests".to_owned()),
            },
            explain: None,
        };

        assert_eq!(
            format_issue_subject(&issue),
            "api:services/api/src/api/main.py:1 requests"
        );
    }

    /// A plugin reference without a position shows no line (#835).
    #[test]
    fn plugin_import_subject_has_no_line() {
        let subject = IssueSubject::Import {
            module: "myapp".to_owned(),
            file: "mysite/settings.py".to_owned(),
            line: None,
            distribution: None,
        };

        assert_eq!(format_subject(&subject), "mysite/settings.py myapp");
    }

    /// Seven warnings of one kind keep five lines and count the rest;
    /// a kind under the limit and a later kind are untouched (#888).
    #[test]
    fn condense_counts_repeats_past_the_limit_per_kind() {
        let mut diagnostics: Vec<String> = (1..=7)
            .map(|n| format!("parse: `tests/t{n}.py:1`: relative import without a package"))
            .collect();
        diagnostics.insert(
            2,
            "resolve: could not read virtualenv `.venv`: gone".to_owned(),
        );

        let condensed = condense_diagnostics(&diagnostics);

        assert_eq!(
            condensed,
            [
                "parse: `tests/t1.py:1`: relative import without a package",
                "parse: `tests/t2.py:1`: relative import without a package",
                "resolve: could not read virtualenv `.venv`: gone",
                "parse: `tests/t3.py:1`: relative import without a package",
                "parse: `tests/t4.py:1`: relative import without a package",
                "parse: `tests/t5.py:1`: relative import without a package",
                "parse: … and 2 more like the above (`--reporter json` lists every warning)",
            ]
        );
    }

    /// Exactly the limit is shown as is, without a count line.
    #[test]
    fn condense_keeps_a_kind_at_the_limit() {
        let diagnostics: Vec<String> = (1..=REPEATS_SHOWN)
            .map(|n| format!("manifest: `packages/p{n}`: Hatch detected"))
            .collect();

        assert_eq!(condense_diagnostics(&diagnostics), diagnostics);
    }

    /// Text outside backticks tells kinds apart; quoted paths and line
    /// numbers do not.
    #[test]
    fn diagnostic_kind_blanks_quoted_spans_and_numbers() {
        assert_eq!(
            diagnostic_kind("parse: `a.py:3`: relative import"),
            diagnostic_kind("parse: `b/c.py:9`: relative import")
        );
        assert_eq!(
            diagnostic_kind(
                "parse: skipped `a.ipynb`: syntax error at line 1: Expected a statement"
            ),
            diagnostic_kind(
                "parse: skipped `b.ipynb`: syntax error at line 96: Expected a statement"
            )
        );
        assert_ne!(
            diagnostic_kind("parse: `a.py:3`: relative import"),
            diagnostic_kind("parse: `a.py:3`: invalid notebook")
        );
        assert_eq!(
            diagnostic_kind("parse: `a.py`: line 12, column 3"),
            "parse: ``: line #, column #"
        );
    }
}
