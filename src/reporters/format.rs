//! Shared formatting helpers for reporters.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;

use crate::manifest::DependencyOrigin;
use crate::rules::{
    Issue, IssueLocation, IssueReport, IssueSubject, RuleId, SuppressReason, import_site,
};

use super::types::Diagnostic;

/// `text` with control, bidi and line-separator characters written as `\u{..}`.
///
/// That is `U+0000`–`U+001F`, `U+007F`–`U+009F`, bidi embedding/override/
/// isolate (`U+202A`–`U+202E`, `U+2066`–`U+2069`) and line/paragraph
/// separator (`U+2028`, `U+2029`).
/// File names, venv METADATA and manifest values come from the analyzed
/// project; printed raw they could colour the terminal, forge a report line
/// (#973) or reorder one (#996). JSON and SARIF go through serde, which
/// escapes only the control characters.
pub fn escape_control(text: &str) -> Cow<'_, str> {
    if !text.chars().any(needs_escape) {
        return Cow::Borrowed(text);
    }
    let mut escaped = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        if needs_escape(ch) {
            escaped.extend(ch.escape_unicode());
        } else {
            escaped.push(ch);
        }
    }
    Cow::Owned(escaped)
}

/// `escape_control` for multi-line text, such as an error that quotes a
/// TOML snippet: each `\n` or `\r\n` is kept as a line break, the rest is
/// escaped.
pub fn escape_control_lines(text: &str) -> Cow<'_, str> {
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    for line in text.split_inclusive('\n') {
        let body = line
            .strip_suffix('\n')
            .map_or(line, |body| body.strip_suffix('\r').unwrap_or(body));
        let escaped = escape_control(body);
        changed |= matches!(escaped, Cow::Owned(_));
        out.push_str(&escaped);
        out.push_str(&line[body.len()..]);
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(text)
    }
}

fn needs_escape(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}'
        )
}

/// Format a primary location column for human reporters.
pub(super) fn format_location_column(location: &IssueLocation) -> String {
    let column = if let Some(origin) = &location.manifest {
        format_manifest_origin(origin)
    } else {
        match (&location.file, location.line) {
            (Some(file), Some(line)) => format!("{file}:{line}"),
            (Some(file), None) => file.clone(),
            (None, _) => "-".to_owned(),
        }
    };
    escape_control(&column).into_owned()
}

fn format_manifest_origin(origin: &DependencyOrigin) -> String {
    match origin.line {
        Some(line) => format!("{}:{line}", origin.file),
        None => origin.file.clone(),
    }
}

/// Short subject label for compact output, control characters escaped.
pub(super) fn format_subject(subject: &IssueSubject) -> String {
    let label = match subject {
        IssueSubject::File { path } => path.clone(),
        IssueSubject::Distribution { name } | IssueSubject::Binary { name } => name.clone(),
        IssueSubject::Symbol { module, name } => format!("{module}:{name}"),
        IssueSubject::Import {
            module, file, line, ..
        } => format!("{} {module}", import_site(file, *line)),
        IssueSubject::ScriptDistribution { script, name } => format!("script:{script}:{name}"),
    };
    escape_control(&label).into_owned()
}

/// Short subject label annotated with workspace member metadata when present.
pub(super) fn format_issue_subject(issue: &Issue) -> String {
    let subject = format_subject(&issue.subject);
    issue.workspace_member.as_ref().map_or_else(
        || subject.clone(),
        |member| format!("{}:{subject}", escape_control(member)),
    )
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
    format!(
        "  {subject:<24} {location:<22} {}",
        escape_control(&issue.message)
    )
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
pub(super) fn write_warning_count(out: &mut String, diagnostics: &[Diagnostic]) {
    if !diagnostics.is_empty() {
        let _ = writeln!(out, "Warnings: {} (see stderr)", diagnostics.len());
    }
}

/// Lines of one warning kind shown before the rest are only counted.
const REPEATS_SHOWN: usize = 5;

/// `diagnostics` with each kind cut to its first [`REPEATS_SHOWN`] warnings
/// and a count of the rest, for outputs a person reads top to bottom (#888).
/// The count points nowhere: it stands for warnings in many files.
pub(crate) fn condense_diagnostics(diagnostics: &[Diagnostic]) -> Vec<Diagnostic> {
    let mut totals = HashMap::<u64, usize>::new();
    for diagnostic in diagnostics {
        *totals.entry(diagnostic.kind).or_default() += 1;
    }
    let mut shown = HashMap::<u64, usize>::new();
    let mut out = Vec::new();
    for diagnostic in diagnostics {
        let seen = shown.entry(diagnostic.kind).or_default();
        *seen += 1;
        if *seen <= REPEATS_SHOWN {
            out.push(diagnostic.clone());
        }
        let total = totals[&diagnostic.kind];
        if *seen == REPEATS_SHOWN && total > REPEATS_SHOWN {
            let area = diagnostic.text.split_once(':').map_or("", |(area, _)| area);
            out.push(Diagnostic {
                kind: diagnostic.kind,
                file: None,
                line: None,
                text: format!(
                    "{area}: … and {} more like the above (`--reporter json` lists every warning)",
                    total - REPEATS_SHOWN
                ),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Confidence;
    use crate::manifest::ManifestWarning;
    use crate::pipeline::{ProbeWarning, SkipReason, diagnostic_lines};
    use crate::resolver::ResolveWarning;
    use crate::rules::{Issue, IssueLocation, RuleId, Severity};

    /// C0, DEL and C1 are spelled out; other text, `\t` aside, is kept (#973).
    #[test]
    fn escape_control_spells_out_control_characters() {
        assert!(matches!(
            escape_control("acme/ok.py"),
            Cow::Borrowed("acme/ok.py")
        ));
        assert_eq!(
            escape_control("a\x1b[31m\nb\tc\x7f\u{9b}é"),
            "a\\u{1b}[31m\\u{a}b\\u{9}c\\u{7f}\\u{9b}é"
        );
    }

    /// Bidi embedding, override and isolate controls and the line/paragraph
    /// separators are spelled out; other format characters are kept (#996).
    #[test]
    fn escape_control_spells_out_bidi_controls_and_separators() {
        assert_eq!(
            escape_control("a\u{202e}b\u{2066}c\u{2028}d\u{2029}e\u{200d}f"),
            "a\\u{202e}b\\u{2066}c\\u{2028}d\\u{2029}e\u{200d}f"
        );
    }

    /// Line breaks survive; everything else on each line is escaped (#996).
    #[test]
    fn escape_control_lines_keeps_line_breaks() {
        assert!(matches!(
            escape_control_lines("a\n  |\nb"),
            Cow::Borrowed("a\n  |\nb")
        ));
        assert_eq!(
            escape_control_lines("a\x1b[31m\n\r\u{202e}b\n"),
            "a\\u{1b}[31m\n\\u{d}\\u{202e}b\n"
        );
        assert!(matches!(
            escape_control_lines("a\r\n  |\r\n"),
            Cow::Borrowed("a\r\n  |\r\n")
        ));
        assert_eq!(escape_control_lines("a\r\nb\r"), "a\r\nb\\u{d}");
    }

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

    fn parse_diagnostic(path: &str, message: &str) -> ProbeWarning {
        ProbeWarning::ParseDiagnostic {
            path: path.to_owned(),
            line: 1,
            message: message.to_owned(),
        }
    }

    fn texts(diagnostics: &[Diagnostic]) -> Vec<&str> {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.text.as_str())
            .collect()
    }

    /// Seven warnings of one kind keep five lines and count the rest;
    /// a kind under the limit and a later kind are untouched (#888).
    #[test]
    fn condense_counts_repeats_past_the_limit_per_kind() {
        let mut warnings: Vec<ProbeWarning> = (1..=7)
            .map(|n| {
                parse_diagnostic(
                    &format!("tests/t{n}.py"),
                    "relative import without a package",
                )
            })
            .collect();
        warnings.insert(
            2,
            ProbeWarning::Resolve(ResolveWarning::VenvUnreadable {
                path: ".venv".to_owned(),
                reason: "gone".to_owned(),
            }),
        );

        let condensed = condense_diagnostics(&diagnostic_lines(&warnings));

        assert_eq!(
            texts(&condensed),
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
        assert_eq!(condensed[5].file.as_deref(), Some("tests/t5.py"));
        assert_eq!(condensed[6].file, None);
    }

    /// Exactly the limit is shown as is, without a count line.
    #[test]
    fn condense_keeps_a_kind_at_the_limit() {
        let warnings: Vec<ProbeWarning> = (1..=REPEATS_SHOWN)
            .map(|n| ProbeWarning::MemberManifest {
                member: format!("packages/p{n}"),
                warning: ManifestWarning::HatchDetected,
            })
            .collect();
        let diagnostics = diagnostic_lines(&warnings);

        assert_eq!(condense_diagnostics(&diagnostics), diagnostics);
    }

    /// The variant tells kinds apart, and a parse diagnostic's message
    /// with it; where a warning points and what a syntax error says do not
    /// (#895).
    #[test]
    fn diagnostic_kind_is_the_variant() {
        let kind = |warning: ProbeWarning| warning.diagnostic().kind;
        let syntax_error = |path: &str, line, message: &str| ProbeWarning::SkippedSource {
            path: path.to_owned(),
            reason: SkipReason::SyntaxError {
                line,
                message: message.to_owned(),
            },
        };

        assert_eq!(
            kind(parse_diagnostic("a.py", "relative import")),
            kind(parse_diagnostic("b/c.py", "relative import"))
        );
        assert_ne!(
            kind(parse_diagnostic("a.py", "relative import")),
            kind(parse_diagnostic(
                "a.py",
                "unsupported `__all__` assignment form"
            ))
        );
        assert_eq!(
            kind(syntax_error("a.ipynb", 1, "Expected a statement")),
            kind(syntax_error("b.py", 96, "unexpected indent"))
        );
        assert_ne!(
            kind(ProbeWarning::Manifest(ManifestWarning::HatchDetected)),
            kind(ProbeWarning::Manifest(ManifestWarning::PdmDetected))
        );
    }
}
