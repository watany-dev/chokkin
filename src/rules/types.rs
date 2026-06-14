//! Shared rule and issue types for pipeline steps 10–12.

use indexmap::IndexSet;

use crate::config::Confidence;
use crate::manifest::DependencyOrigin;
use crate::plugins::ReferenceOrigin;

/// CHK001–CHK010 rule identifiers (§3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuleId {
    /// Unused file (entry unreachable).
    Chk001,
    /// Unused declared dependency.
    Chk002,
    /// Missing direct dependency declaration.
    Chk003,
    /// Transitive-only dependency import.
    Chk004,
    /// Misplaced dependency (context mismatch).
    Chk005,
    /// Unused public export.
    Chk006,
    /// Unused re-export.
    Chk007,
    /// Unlisted binary dependency.
    Chk008,
    /// Duplicate dependency declaration.
    Chk009,
    /// Unresolved import.
    Chk010,
}

impl RuleId {
    /// Stable `CHK00x` code for reporters and `--explain`.
    #[must_use]
    pub const fn as_code(self) -> &'static str {
        match self {
            Self::Chk001 => "CHK001",
            Self::Chk002 => "CHK002",
            Self::Chk003 => "CHK003",
            Self::Chk004 => "CHK004",
            Self::Chk005 => "CHK005",
            Self::Chk006 => "CHK006",
            Self::Chk007 => "CHK007",
            Self::Chk008 => "CHK008",
            Self::Chk009 => "CHK009",
            Self::Chk010 => "CHK010",
        }
    }

    /// Parse a `CHK00x` selector for `--explain`.
    pub fn parse_code(value: &str) -> Option<Self> {
        match value.to_ascii_uppercase().as_str() {
            "CHK001" => Some(Self::Chk001),
            "CHK002" => Some(Self::Chk002),
            "CHK003" => Some(Self::Chk003),
            "CHK004" => Some(Self::Chk004),
            "CHK005" => Some(Self::Chk005),
            "CHK006" => Some(Self::Chk006),
            "CHK007" => Some(Self::Chk007),
            "CHK008" => Some(Self::Chk008),
            "CHK009" => Some(Self::Chk009),
            "CHK010" => Some(Self::Chk010),
            _ => None,
        }
    }
}

/// Issue severity for exit-code and reporter filtering (§3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Counts toward exit code 1 by default.
    Error,
    /// Reported; exit 1 only in strict mode for some rules.
    Warning,
    /// Informational (e.g. optional try-import missing).
    Info,
}

/// Subject of an issue candidate or final issue.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IssueSubject {
    /// Root-relative file path.
    File {
        /// Path using `/` separators.
        path: String,
    },
    /// PEP 508 distribution name.
    Distribution {
        /// Normalized distribution name.
        name: String,
    },
    /// Public symbol in a module.
    Symbol {
        /// Dotted module name.
        module: String,
        /// Symbol name.
        name: String,
    },
    /// CLI binary name.
    Binary {
        /// Binary executable name.
        name: String,
    },
    /// Import reference.
    Import {
        /// Imported module.
        module: String,
        /// Source file path.
        file: String,
        /// 1-based line number.
        line: u32,
    },
}

/// Where evidence for an issue was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Manifest dependency declaration.
    Manifest(DependencyOrigin),
    /// Python import in source.
    Import {
        /// Source file path.
        file: String,
        /// 1-based line number.
        line: u32,
        /// Imported module name.
        module: String,
    },
    /// CLI binary usage from a plugin.
    Binary(ReferenceOrigin),
    /// Configuration module reference.
    Config(ReferenceOrigin),
}

/// Structured data for `--explain` (Step 12).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExplainData {
    /// One-line summary of the finding.
    pub summary: String,
    /// Additional detail lines.
    pub details: Vec<String>,
}

/// Pre-issue candidate from Steps 10–11 before ignore/filter (Step 12 input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCandidate {
    /// Rule that produced this candidate.
    pub rule: RuleId,
    /// Primary subject.
    pub subject: IssueSubject,
    /// Default severity before strict overrides.
    pub severity: Severity,
    /// Confidence in the finding.
    pub confidence: Confidence,
    /// Human-readable message.
    pub message: String,
    /// Evidence locations.
    pub origins: Vec<Origin>,
    /// Explain payload for `--explain`.
    pub explain: ExplainData,
}

/// Non-fatal diagnostic from dependency reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileDiagnostic {
    /// Diagnostic message.
    pub message: String,
}

/// Output of pipeline step 10.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DependencyReport {
    /// Issue candidates for Step 12.
    pub candidates: Vec<IssueCandidate>,
    /// Distributions considered used during reconciliation.
    pub used_distributions: IndexSet<String>,
    /// Non-fatal reconciliation notes.
    pub diagnostics: Vec<ReconcileDiagnostic>,
}

/// Final issue location for reporters and `--explain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueLocation {
    /// Root-relative source file when applicable.
    pub file: Option<String>,
    /// 1-based line number when applicable.
    pub line: Option<u32>,
    /// Manifest declaration location when applicable.
    pub manifest: Option<DependencyOrigin>,
}

/// Final issue after emission filters (pipeline step 12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Rule that produced this issue.
    pub rule: RuleId,
    /// Issue severity.
    pub severity: Severity,
    /// Confidence in the finding.
    pub confidence: Confidence,
    /// Human-readable message.
    pub message: String,
    /// Primary evidence location.
    pub location: IssueLocation,
    /// Issue subject.
    pub subject: IssueSubject,
    /// Explain payload for `--explain`.
    pub explain: Option<ExplainData>,
}

/// Why an issue was suppressed by ignore rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressReason {
    /// Matched `[tool.chokkin.ignore]`.
    Config,
    /// Matched inline `# chokkin: ignore[…]`.
    Inline,
    /// Matched file-level `# chokkin: file-ignore[…]`.
    FileLevel,
}

/// Issue suppressed by ignore configuration or directives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppressedIssue {
    /// The suppressed issue.
    pub issue: Issue,
    /// Why it was suppressed.
    pub reason: SuppressReason,
}

/// Per-rule issue counts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IssueSummary {
    /// Total reported issues.
    pub total: u32,
    /// Counts keyed by rule id.
    pub by_rule: std::collections::BTreeMap<RuleId, u32>,
}

/// Output of pipeline step 12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueReport {
    /// Issues that passed filters and ignore rules.
    pub issues: Vec<Issue>,
    /// Issues suppressed by ignore rules (`--debug` output).
    pub suppressed: Vec<SuppressedIssue>,
    /// Aggregate statistics.
    pub summary: IssueSummary,
    /// Recommended process exit status.
    pub exit_status: crate::ExitStatus,
}

impl IssueReport {
    /// Empty report with success exit status.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            issues: Vec::new(),
            suppressed: Vec::new(),
            summary: IssueSummary::default(),
            exit_status: crate::ExitStatus::Success,
        }
    }
}

/// Stable sort key for issue candidates within a rule.
pub(super) fn subject_sort_key(subject: &IssueSubject) -> String {
    match subject {
        IssueSubject::Distribution { name } | IssueSubject::Binary { name } => name.clone(),
        IssueSubject::File { path } => path.clone(),
        IssueSubject::Symbol { module, name } => format!("{module}:{name}"),
        IssueSubject::Import { module, file, line } => format!("{file}:{line}:{module}"),
    }
}
