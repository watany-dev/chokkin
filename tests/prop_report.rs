//! Property-based tests for reporters and baseline round-trips.

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use chokkin::internals::ProjectMode;
use chokkin::internals::{
    Confidence, Issue, IssueLocation, IssueReport, IssueSubject, ReporterId, RuleId, Severity,
    apply_baseline, issue_fingerprint, write_baseline,
};
use chokkin::{ExitStatus, RenderContext, RuntimeOverrides, render_issues};
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
            proptest::option::of(1u32..500),
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

fn severity() -> impl Strategy<Value = Severity> {
    prop::sample::select(vec![Severity::Error, Severity::Warning, Severity::Info])
}

fn confidence() -> impl Strategy<Value = Confidence> {
    prop::sample::select(vec![
        Confidence::Certain,
        Confidence::Likely,
        Confidence::Maybe,
    ])
}

fn issue() -> impl Strategy<Value = Issue> {
    (
        prop::sample::select(RuleId::ALL.to_vec()),
        severity(),
        confidence(),
        nasty_text(),
        subject(),
        proptest::option::of(path_text()),
        proptest::option::of(1u32..500),
    )
        .prop_map(
            |(rule, severity, confidence, message, subject, file, line)| Issue {
                rule,
                severity,
                confidence,
                message,
                workspace_member: None,
                location: IssueLocation {
                    file,
                    line,
                    manifest: None,
                },
                subject,
                explain: None,
            },
        )
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

static REPORT_SCHEMA: std::sync::LazyLock<jsonschema::Validator> = std::sync::LazyLock::new(|| {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs/schema/chokkin-report.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read schema"))
            .expect("parse schema");
    jsonschema::Validator::new(&schema).expect("compile schema")
});

/// The cells of a GFM table row: split on each `|` not escaped as `\|`,
/// without the outer pipes, each trimmed.
fn table_cells(row: &str) -> Vec<&str> {
    let mut cells = Vec::new();
    let mut start = 0;
    let bytes = row.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'|' && (index == 0 || bytes[index - 1] != b'\\') {
            cells.push(row[start..index].trim());
            start = index + 1;
        }
    }
    cells.push(row[start..].trim());
    // Text before the first pipe and after the last one is no cell.
    if cells.first() == Some(&"") {
        cells.remove(0);
    }
    if cells.last() == Some(&"") {
        cells.pop();
    }
    cells
}

/// Whether `cell` is exactly one `CommonMark` code span: a backtick run, then
/// text holding no run of that length, then the same run.
fn is_code_span(cell: &str) -> bool {
    let fence = cell.len() - cell.trim_start_matches('`').len();
    if fence == 0 || cell.len() < 2 * fence + 1 {
        return false;
    }
    let inner = &cell[fence..cell.len() - fence];
    if !cell.ends_with(&"`".repeat(fence)) || inner.starts_with('`') || inner.ends_with('`') {
        return false;
    }
    !inner.split(|ch| ch != '`').any(|run| run.len() == fence)
}

/// The issue fields a JSON consumer reads back.
#[derive(Debug, PartialEq, Eq)]
struct JsonIssue {
    code: String,
    severity: String,
    confidence: String,
    message: String,
    fingerprint: String,
    file: Option<String>,
    line: Option<u64>,
    path: Option<String>,
    distribution: Option<String>,
    symbol: Option<String>,
    binary: Option<String>,
}

fn json_issue(value: &serde_json::Value) -> JsonIssue {
    let text = |key: &str| value[key].as_str().map(str::to_owned);
    JsonIssue {
        code: text("code").unwrap_or_default(),
        severity: text("severity").unwrap_or_default(),
        confidence: text("confidence").unwrap_or_default(),
        message: text("message").unwrap_or_default(),
        fingerprint: text("fingerprint").unwrap_or_default(),
        file: text("file"),
        line: value["line"].as_u64(),
        path: text("path"),
        distribution: text("distribution"),
        symbol: text("symbol"),
        binary: text("binary"),
    }
}

fn expected_json_issue(issue: &Issue) -> JsonIssue {
    let (path, distribution, symbol, binary) = match &issue.subject {
        IssueSubject::File { path } => (Some(path.clone()), None, None, None),
        IssueSubject::Distribution { name } => (None, Some(name.clone()), None, None),
        IssueSubject::Binary { name } => (None, None, None, Some(name.clone())),
        IssueSubject::Symbol { module, name } => {
            (None, None, Some(format!("{module}:{name}")), None)
        },
        IssueSubject::Import {
            module,
            file,
            distribution,
            ..
        } => (
            Some(file.clone()),
            distribution.clone(),
            Some(module.clone()),
            None,
        ),
        IssueSubject::ScriptDistribution { script, name } => {
            (Some(script.clone()), Some(name.clone()), None, None)
        },
    };
    JsonIssue {
        code: issue.rule.as_code().to_owned(),
        severity: match issue.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
        .to_owned(),
        confidence: issue.confidence.as_str().to_owned(),
        message: issue.message.clone(),
        fingerprint: issue_fingerprint(issue),
        file: issue.location.file.clone(),
        line: issue.location.line.map(u64::from),
        path,
        distribution,
        symbol,
        binary,
    }
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

    /// Markdown table rows stay rows (#1105): whatever `|`, newline or
    /// backtick a name or message holds, each issue is one line of four
    /// cells, and the subject and location are each one whole code span.
    #[test]
    fn markdown_rows_keep_four_cells(issues in prop::collection::vec(issue(), 0..6)) {
        let report = report(issues);
        let out = render_issues(ReporterId::Markdown, &report, &context());
        let rows: Vec<&str> = out.lines().filter(|line| line.starts_with("| CHK")).collect();
        prop_assert_eq!(rows.len(), report.issues.len(), "{}", out);
        for row in rows {
            let cells = table_cells(row);
            prop_assert_eq!(cells.len(), 4, "{:?}", row);
            prop_assert!(is_code_span(cells[1]), "subject {:?} in {:?}", cells[1], row);
            prop_assert!(is_code_span(cells[2]), "location {:?} in {:?}", cells[2], row);
        }
    }

    /// The compact reporter prints one line per issue, in report order.
    #[test]
    fn compact_reporter_is_one_line_per_issue(issues in prop::collection::vec(issue(), 1..6)) {
        let report = report(issues);
        let out = render_issues(ReporterId::Compact, &report, &context());
        let codes: Vec<&str> = out.lines().map(|line| line.split(' ').next().unwrap_or("")).collect();
        let want: Vec<&str> = report.issues.iter().map(|issue| issue.rule.as_code()).collect();
        prop_assert_eq!(codes, want, "{}", out);
    }

    /// The default reporter prints each issue on one indented line under
    /// its group title.
    #[test]
    fn default_reporter_is_one_line_per_issue(issues in prop::collection::vec(issue(), 0..6)) {
        let report = report(issues);
        let out = render_issues(ReporterId::Default, &report, &context());
        let issue_lines = out.lines().filter(|line| line.starts_with("  ")).count();
        prop_assert_eq!(issue_lines, report.issues.len(), "{}", out);
        let groups = report.summary.by_rule.len();
        // Version, blank, project, mode, blank; per group a title, its
        // issues and a blank; the summary.
        prop_assert_eq!(out.lines().count(), 5 + 2 * groups + report.issues.len() + 1, "{}", out);
    }

    /// JSON output matches the published schema and reads back as the
    /// issues it was rendered from.
    #[test]
    fn json_reporter_reads_back_against_the_schema(
        issues in prop::collection::vec(issue(), 0..6),
    ) {
        let report = report(issues);
        let out = render_issues(ReporterId::Json, &report, &context());
        let value: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        let errors: Vec<String> = REPORT_SCHEMA
            .iter_errors(&value)
            .map(|error| error.to_string())
            .collect();
        prop_assert!(errors.is_empty(), "{:?}", errors);
        let read: Vec<JsonIssue> = value["issues"]
            .as_array()
            .expect("issues array")
            .iter()
            .map(json_issue)
            .collect();
        let want: Vec<JsonIssue> = report.issues.iter().map(expected_json_issue).collect();
        prop_assert_eq!(read, want);
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

/// What a later run may change about the same finding: everything but the
/// rule, the subject, the evidence file of a symbol, and the member.
#[derive(Debug, Clone)]
struct Drift {
    severity: Severity,
    confidence: Confidence,
    message: String,
    line: Option<u32>,
    import_line: Option<u32>,
    distribution: Option<String>,
}

fn drift() -> impl Strategy<Value = Drift> {
    (
        severity(),
        confidence(),
        nasty_text(),
        proptest::option::of(1u32..500),
        proptest::option::of(1u32..500),
        proptest::option::of(nasty_text()),
    )
        .prop_map(
            |(severity, confidence, message, line, import_line, distribution)| Drift {
                severity,
                confidence,
                message,
                line,
                import_line,
                distribution,
            },
        )
}

/// `issue` as a later run may report it: v0.7 moved CHK005 between
/// confidences and severities, reworded messages and moved import sites
/// without breaking a baseline.
fn drifted(issue: &Issue, drift: &Drift) -> Issue {
    let mut next = issue.clone();
    next.severity = drift.severity;
    next.confidence = drift.confidence;
    next.message.clone_from(&drift.message);
    next.location.line = drift.line;
    if !matches!(next.subject, IssueSubject::Symbol { .. }) {
        // Only a symbol's target is its evidence file.
        next.location.file = None;
    }
    if let IssueSubject::Import {
        line, distribution, ..
    } = &mut next.subject
    {
        *line = drift.import_line;
        distribution.clone_from(&drift.distribution);
    }
    next
}

/// A name as rules produce them: no `:` or `/`, which the target joins with.
fn ident() -> impl Strategy<Value = String> + Clone {
    "[a-z_][a-z0-9_.-]{0,6}"
}

/// The parts a fingerprint is a function of, one per subject kind.
#[derive(Debug, Clone)]
enum Key {
    File(String),
    Distribution(String),
    Binary(String),
    Symbol(String, String),
    Import(String, String),
    Script(String, String),
}

fn key(kind: u8) -> BoxedStrategy<Key> {
    match kind {
        0 => path_text().prop_map(Key::File).boxed(),
        1 => prop_oneof![
            ident().prop_map(Key::Distribution),
            ident().prop_map(Key::Binary),
        ]
        .boxed(),
        2 => (path_text(), ident())
            .prop_map(|(file, name)| Key::Symbol(file, name))
            .boxed(),
        3 => (path_text(), ident())
            .prop_map(|(file, module)| Key::Import(file, module))
            .boxed(),
        _ => (path_text(), ident())
            .prop_map(|(script, name)| Key::Script(script, name))
            .boxed(),
    }
}

/// Two values from `strategy`, equal about half the time.
fn pair<T: Clone + std::fmt::Debug + 'static>(
    strategy: impl Strategy<Value = T> + Clone + 'static,
) -> impl Strategy<Value = (T, T)> {
    strategy.clone().prop_flat_map(move |first| {
        (
            Just(first.clone()),
            prop_oneof![Just(first).boxed(), strategy.clone().boxed()],
        )
    })
}

/// Two keys of one kind, equal about half the time.
fn key_pair() -> impl Strategy<Value = (Key, Key)> {
    (0u8..5).prop_flat_map(|kind| pair(key(kind)))
}

fn keyed(rule: RuleId, member: Option<String>, key: &Key) -> Issue {
    let (subject, file) = match key.clone() {
        Key::File(path) => (IssueSubject::File { path }, None),
        Key::Distribution(name) => (IssueSubject::Distribution { name }, None),
        Key::Binary(name) => (IssueSubject::Binary { name }, None),
        Key::Symbol(file, name) => (
            IssueSubject::Symbol {
                module: "pkg.mod".to_owned(),
                name,
            },
            Some(file),
        ),
        Key::Import(file, module) => (
            IssueSubject::Import {
                module,
                file,
                line: Some(1),
                distribution: None,
            },
            None,
        ),
        Key::Script(script, name) => (IssueSubject::ScriptDistribution { script, name }, None),
    };
    Issue {
        rule,
        severity: Severity::Warning,
        confidence: Confidence::Certain,
        message: String::new(),
        workspace_member: member,
        location: IssueLocation {
            file,
            line: Some(1),
            manifest: None,
        },
        subject,
        explain: None,
    }
}

/// What the fingerprint keys on within one kind. A distribution and a binary
/// spell their target alike, and no rule reports both, so they are one kind.
fn target_parts(key: &Key) -> Vec<&str> {
    match key {
        Key::File(path) | Key::Distribution(path) | Key::Binary(path) => vec![path],
        Key::Symbol(file, name) | Key::Import(file, name) | Key::Script(file, name) => {
            vec![file, name]
        },
    }
}

proptest! {
    /// Every 0.7.x release kept baselines matching while it changed
    /// severities, confidences, messages and lines: the fingerprint is a
    /// function of the rule, the subject and the member only.
    #[test]
    fn fingerprint_ignores_everything_but_rule_subject_and_member(
        issue in issue(),
        drift in drift(),
    ) {
        prop_assert_eq!(issue_fingerprint(&drifted(&issue, &drift)), issue_fingerprint(&issue));
    }

    /// Two findings that differ in rule, member, or subject never share a
    /// fingerprint, so a baseline cannot hide a new finding.
    #[test]
    fn fingerprint_separates_rule_subject_and_member(
        rules in pair(prop::sample::select(RuleId::ALL.to_vec())),
        members in pair(proptest::option::of(ident())),
        keys in key_pair(),
    ) {
        let same = rules.0 == rules.1
            && members.0 == members.1
            && target_parts(&keys.0) == target_parts(&keys.1);
        let left = keyed(rules.0, members.0.clone(), &keys.0);
        let right = keyed(rules.1, members.1, &keys.1);
        prop_assert_eq!(issue_fingerprint(&left) == issue_fingerprint(&right), same);
    }

    /// A baseline written by one run suppresses the next run's report of the
    /// same findings however their confidence and wording changed, and the
    /// suppressed issues keep what the new run said about them.
    #[test]
    fn baseline_survives_drift_and_keeps_new_confidence(
        cases in prop::collection::vec((issue(), drift()), 1..6),
    ) {
        let dir = TempDir::new().expect("tempdir");
        let before: Vec<Issue> = cases.iter().map(|(issue, _)| issue.clone()).collect();
        let after: Vec<Issue> = cases.iter().map(|(issue, drift)| drifted(issue, drift)).collect();
        write_baseline(&report(before), dir.path(), std::path::Path::new("b.json")).expect("write");

        let mut applied = report(after.clone());
        apply_baseline(&mut applied, dir.path(), std::path::Path::new("b.json"), &RuntimeOverrides::default())
            .expect("apply");
        prop_assert!(applied.issues.is_empty());
        let suppressed: Vec<Issue> = applied.suppressed.into_iter().map(|s| s.issue).collect();
        prop_assert_eq!(suppressed, after);
    }
}
