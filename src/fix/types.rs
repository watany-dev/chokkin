//! Fix types for pipeline step 13.

use std::fmt;

use crate::manifest::LoadedManifest;
use crate::rules::{IssueSubject, RuleId};

/// Options controlling automatic fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FixOptions {
    /// When true, skip writing files (dry-run).
    pub dry_run: bool,
    /// Allow explicit deletion of unreachable project files.
    pub allow_remove_files: bool,
    /// Request missing-dependency insertion when it becomes unambiguous.
    pub add_missing: bool,
}

/// Member manifest metadata available to workspace-aware fixes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkspaceFixManifest<'a> {
    /// Stable workspace member id.
    pub id: &'a str,
    /// Member directory path relative to the project root.
    pub path: &'a str,
    /// Root-relative member `pyproject.toml` path when present.
    pub pyproject_toml: Option<&'a str>,
    /// Member-local manifest extraction.
    pub manifest: &'a LoadedManifest,
}

/// One successfully applied manifest edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedFix {
    /// Rule that triggered the fix.
    pub rule: RuleId,
    /// Issue subject that was fixed.
    pub subject: IssueSubject,
    /// Root-relative file that was edited.
    pub file: String,
    /// Human-readable description of the change.
    pub description: String,
}

/// Why a fix was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkippedReason {
    /// Rule or confidence is outside the safe fix contract.
    NotFixable,
    /// File type or location is not supported.
    UnsupportedTarget,
    /// `--allow-remove-files` was required but not set.
    FileRemovalDenied,
    /// Missing manifest metadata needed to apply the fix.
    MissingOrigin,
    /// Edit could not be applied without ambiguity.
    Ambiguous,
}

impl fmt::Display for SkippedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFixable => "not-fixable",
            Self::UnsupportedTarget => "unsupported-target",
            Self::FileRemovalDenied => "file-removal-denied",
            Self::MissingOrigin => "missing-origin",
            Self::Ambiguous => "ambiguous",
        })
    }
}

/// A fix that was not applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFix {
    /// Rule that was considered.
    pub rule: RuleId,
    /// Issue subject.
    pub subject: IssueSubject,
    /// Why the fix was skipped.
    pub reason: SkippedReason,
    /// Additional detail for reporters.
    pub detail: String,
}

/// Outcome of optional fix application.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FixReport {
    /// True when `applied` holds previews from `--dry-run` rather than written edits.
    pub dry_run: bool,
    /// Applied manifest edits.
    pub applied: Vec<AppliedFix>,
    /// Skipped fixes with reasons.
    pub skipped: Vec<SkippedFix>,
    /// Post-fix reminders (e.g. refresh lockfile).
    pub reminders: Vec<String>,
}
