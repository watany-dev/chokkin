//! Stable CHK rule metadata for reporters (Phase 3 / v0.3).

use super::types::{RuleId, Severity};

/// Short human-readable title for a rule.
#[must_use]
pub fn rule_title(rule: RuleId) -> &'static str {
    match rule {
        RuleId::Chk001 => "unused file",
        RuleId::Chk002 => "unused dependency",
        RuleId::Chk003 => "missing dependency",
        RuleId::Chk004 => "transitive dependency",
        RuleId::Chk005 => "misplaced dependency",
        RuleId::Chk006 => "unused export",
        RuleId::Chk007 => "unused re-export",
        RuleId::Chk008 => "unlisted binary",
        RuleId::Chk009 => "duplicate dependency",
        RuleId::Chk010 => "unresolved import",
    }
}

/// Longer help text for SARIF and documentation consumers.
#[must_use]
pub fn rule_help_text(rule: RuleId) -> &'static str {
    match rule {
        RuleId::Chk001 => {
            "A Python file is not reachable from any configured entry root in app mode."
        },
        RuleId::Chk002 => {
            "A declared dependency is not used by imports, binaries, plugins, or config references."
        },
        RuleId::Chk003 => {
            "Source imports a distribution that is not declared in the project manifest."
        },
        RuleId::Chk004 => {
            "Source imports a distribution that is available only through another dependency."
        },
        RuleId::Chk005 => {
            "A dependency is declared in a context that does not match how it is used."
        },
        RuleId::Chk006 => {
            "A public symbol is exported but not referenced outside its defining module."
        },
        RuleId::Chk007 => "A re-exported symbol is not referenced outside its defining module.",
        RuleId::Chk008 => "A CLI binary is used but its owning distribution is not declared.",
        RuleId::Chk009 => "The same distribution is declared more than once in the manifest.",
        RuleId::Chk010 => {
            "An import could not be resolved to first-party, workspace, or third-party code."
        },
    }
}

/// Stable help URI for SARIF rule metadata.
#[must_use]
pub fn rule_help_uri(rule: RuleId) -> String {
    format!(
        "https://github.com/watany-dev/chokkin/blob/main/docs/dev/spec.ja.md#{}",
        rule.as_code().to_ascii_lowercase()
    )
}

/// Default severity before config overrides (§3).
#[must_use]
pub fn default_rule_severity(rule: RuleId) -> Severity {
    match rule {
        RuleId::Chk002 | RuleId::Chk003 | RuleId::Chk004 => Severity::Error,
        RuleId::Chk001
        | RuleId::Chk005
        | RuleId::Chk006
        | RuleId::Chk007
        | RuleId::Chk008
        | RuleId::Chk009
        | RuleId::Chk010 => Severity::Warning,
    }
}
