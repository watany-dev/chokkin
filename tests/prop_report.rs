//! Property-based tests for reporters and baseline round-trips.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use chokkin::config::ProjectMode;
use chokkin::{
    Confidence, ExitStatus, Issue, IssueLocation, IssueReport, IssueSubject, RenderContext,
    ReporterId, RuleId, RuntimeOverrides, Severity, apply_baseline, issue_fingerprint,
    render_issues, write_baseline,
};
use proptest::prelude::*;
use tempfile::TempDir;

/// Text that stresses escaping: quotes, backslashes, newlines, unicode, markup.
fn nasty_text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_./\\\\\"'<>&%|`*\\[\\]:,;\\n\\r\\t é日本👍-]{0,16}"
}

fn path_text() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,6}(/[a-z][a-z0-9_]{0,6}){0,3}\\.py"
}

fn subject() -> impl Strategy<Value = IssueSubject> {
    prop_oneof![
        path_text().prop_map(|path| IssueSubject::File { path }),
        nasty_text().prop_map(|name| IssueSubject::Distribution { name }),
        nasty_text().prop_map(|name| IssueSubject::Binary { name }),
        (nasty_text(), nasty_text())
            .prop_map(|(module, name)| IssueSubject::Symbol { module, name }),
        (
            nasty_text(),
            path_text(),
            1u32..500,
            proptest::option::of(nasty_text())
        )
            .prop_map(|(module, file, line, distribution)| IssueSubject::Import {
                module,
                file,
                line,
                distribution
            }),
    ]
}

fn issue() -> impl Strategy<Value = Issue> {
    (
        prop::sample::select(RuleId::ALL.to_vec()),
        prop::sample::select(vec![Severity::Error, Severity::Warning, Severity::Info]),
        nasty_text(),
        subject(),
        proptest::option::of(path_text()),
        proptest::option::of(1u32..500),
    )
        .prop_map(|(rule, severity, message, subject, file, line)| Issue {
            rule,
            severity,
            confidence: Confidence::Certain,
            message,
            workspace_member: None,
            location: IssueLocation {
                file,
                line,
                manifest: None,
            },
            subject,
            explain: None,
        })
}

fn context() -> RenderContext {
    RenderContext {
        project_name: Some("proj \"x\"".to_owned()),
        mode: ProjectMode::App,
        production: false,
        version: "0.0.0",
        config_label: None,
        diagnostics: Vec::new(),
        files: None,
    }
}

fn report(issues: Vec<Issue>) -> IssueReport {
    let mut report = IssueReport::empty();
    report.summary.total = u32::try_from(issues.len()).unwrap_or(u32::MAX);
    for issue in &issues {
        *report.summary.by_rule.entry(issue.rule).or_default() += 1;
    }
    report.exit_status = if issues.is_empty() {
        ExitStatus::Success
    } else {
        ExitStatus::IssuesFound
    };
    report.issues = issues;
    report
}

const ALL_REPORTERS: [ReporterId; 6] = [
    ReporterId::Default,
    ReporterId::Compact,
    ReporterId::Json,
    ReporterId::Markdown,
    ReporterId::Github,
    ReporterId::Sarif,
];

proptest! {
    /// Every reporter renders arbitrary issue text without panicking.
    #[test]
    fn all_reporters_are_total(issues in prop::collection::vec(issue(), 0..6)) {
        let report = report(issues);
        for id in ALL_REPORTERS {
            let _ = render_issues(id, &report, &context());
        }
    }

    /// JSON output is always valid JSON whose issue count matches the report.
    #[test]
    fn json_reporter_emits_valid_json(issues in prop::collection::vec(issue(), 0..6)) {
        let report = report(issues);
        let out = render_issues(ReporterId::Json, &report, &context());
        let value: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        prop_assert_eq!(value["issues"].as_array().map(Vec::len), Some(report.issues.len()));
        prop_assert_eq!(value["summary"]["total"].as_u64(), Some(report.issues.len() as u64));
    }

    /// SARIF output is always valid JSON with one result per issue.
    #[test]
    fn sarif_reporter_emits_valid_json(issues in prop::collection::vec(issue(), 0..6)) {
        let report = report(issues);
        let out = render_issues(ReporterId::Sarif, &report, &context());
        let value: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        prop_assert_eq!(
            value["runs"][0]["results"].as_array().map(Vec::len),
            Some(report.issues.len())
        );
    }

    /// GitHub annotations stay one-per-line: message text can never inject
    /// an extra workflow command through a newline.
    #[test]
    fn github_reporter_is_one_line_per_issue(issues in prop::collection::vec(issue(), 0..6)) {
        let report = report(issues);
        let out = render_issues(ReporterId::Github, &report, &context());
        prop_assert_eq!(out.lines().filter(|l| !l.is_empty()).count(), report.issues.len(), "{:?}", out);
    }

    /// `write_baseline` then `apply_baseline` on the same report suppresses
    /// every issue and yields a clean exit status.
    #[test]
    fn baseline_round_trip_suppresses_everything(issues in prop::collection::vec(issue(), 0..6)) {
        let dir = TempDir::new().expect("tempdir");
        let original = report(issues);
        let written = write_baseline(&original, dir.path(), std::path::Path::new("baseline.json"))
            .expect("write");
        prop_assert_eq!(written.written as usize, original.issues.len());

        let mut applied = original.clone();
        let result = apply_baseline(
            &mut applied,
            dir.path(),
            std::path::Path::new("baseline.json"),
            &RuntimeOverrides::default(),
        )
        .expect("apply");
        prop_assert!(applied.issues.is_empty());
        prop_assert_eq!(applied.suppressed.len(), original.issues.len());
        prop_assert_eq!(result.suppressed as usize, original.issues.len());
        prop_assert_eq!(applied.summary.total, 0);
        prop_assert_eq!(applied.exit_status, ExitStatus::Success);
    }

    /// Applying a baseline never drops or invents issues: kept + suppressed
    /// equals the input, and only fingerprints in the baseline are removed.
    #[test]
    fn baseline_partitions_issues(
        issues in prop::collection::vec(issue(), 1..8),
        mask in prop::collection::vec(any::<bool>(), 8),
    ) {
        let dir = TempDir::new().expect("tempdir");
        let original = report(issues.clone());
        let frozen: Vec<Issue> = issues
            .iter()
            .zip(mask.iter().cycle())
            .filter(|(_, keep)| **keep)
            .map(|(issue, _)| issue.clone())
            .collect();
        write_baseline(&report(frozen.clone()), dir.path(), std::path::Path::new("b.json")).expect("write");

        let mut applied = original.clone();
        apply_baseline(&mut applied, dir.path(), std::path::Path::new("b.json"), &RuntimeOverrides::default()).expect("apply");
        prop_assert_eq!(applied.issues.len() + applied.suppressed.len(), original.issues.len());
        let frozen_fp: std::collections::BTreeSet<_> = frozen.iter().map(issue_fingerprint).collect();
        for kept in &applied.issues {
            prop_assert!(!frozen_fp.contains(&issue_fingerprint(kept)));
        }
        for gone in &applied.suppressed {
            prop_assert!(frozen_fp.contains(&issue_fingerprint(&gone.issue)));
        }
        prop_assert_eq!(applied.summary.total as usize, applied.issues.len());
    }

    /// Baseline paths that climb out of the root are always rejected.
    #[test]
    fn baseline_path_cannot_escape_root(depth in 1usize..4, name in "[a-z]{1,6}") {
        let dir = TempDir::new().expect("tempdir");
        let escaping = format!("{}{name}.json", "../".repeat(depth));
        let result = write_baseline(&IssueReport::empty(), dir.path(), std::path::Path::new(&escaping));
        prop_assert!(result.is_err());
    }
}
