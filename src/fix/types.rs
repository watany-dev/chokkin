//! Fix types for pipeline step 13.

use crate::rules::{IssueSubject, RuleId};

/// Options controlling automatic fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FixOptions {
    /// When true, skip writing files (dry-run).
    pub dry_run: bool,
    /// Reserved for future file deletion support (always rejected in v0.1).
    pub allow_remove_files: bool,
    /// Reserved for missing-dependency insertion (not implemented in v0.1).
    pub add_missing: bool,
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
    /// Requirements line is hash-pinned.
    PinnedRequirements,
    /// `--allow-remove-files` was required but not set.
    FileRemovalDenied,
    /// Missing manifest metadata needed to apply the fix.
    MissingOrigin,
    /// Edit could not be applied without ambiguity.
    Ambiguous,
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
    /// Applied manifest edits.
    pub applied: Vec<AppliedFix>,
    /// Skipped fixes with reasons.
    pub skipped: Vec<SkippedFix>,
    /// Post-fix reminders (e.g. refresh lockfile).
    pub reminders: Vec<String>,
}
