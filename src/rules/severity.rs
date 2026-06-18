//! Per-rule severity resolution from config overrides (Phase 3 / v0.3).

use crate::config::{ChokkinConfig, SeverityLevel};

use super::types::{IssueCandidate, RuleId, Severity};

/// Apply `[tool.chokkin.severity]` overrides to a candidate severity.
///
/// Returns `None` when the rule is configured as `off`.
#[must_use]
pub fn resolve_issue_severity(
    rule: RuleId,
    candidate_severity: Severity,
    config: &ChokkinConfig,
) -> Option<Severity> {
    let Some(level) = config.severity.get(rule.as_code()) else {
        return Some(candidate_severity);
    };
    match level {
        SeverityLevel::Off => None,
        SeverityLevel::Info => Some(Severity::Info),
        SeverityLevel::Warning => Some(Severity::Warning),
        SeverityLevel::Error => Some(Severity::Error),
    }
}

/// Apply severity overrides to a candidate, returning `None` when disabled.
#[must_use]
pub fn apply_severity_override(
    candidate: IssueCandidate,
    config: &ChokkinConfig,
) -> Option<IssueCandidate> {
    let severity = resolve_issue_severity(candidate.rule, candidate.severity, config)?;
    Some(IssueCandidate {
        severity,
        ..candidate
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::rules::default_rule_severity;
    use crate::rules::types::{ExplainData, IssueSubject};

    fn candidate(rule: RuleId) -> IssueCandidate {
        IssueCandidate {
            rule,
            subject: IssueSubject::Distribution {
                name: "pkg".to_owned(),
            },
            severity: default_rule_severity(rule),
            confidence: crate::config::Confidence::Certain,
            message: "test".to_owned(),
            workspace_member: None,
            origins: Vec::new(),
            explain: ExplainData::default(),
        }
    }

    #[test]
    fn off_severity_disables_rule() {
        let mut config = default_config();
        config
            .severity
            .insert("CHK002".to_owned(), SeverityLevel::Off);
        assert!(apply_severity_override(candidate(RuleId::Chk002), &config).is_none());
    }

    #[test]
    fn info_override_downgrades_error_rule() {
        let mut config = default_config();
        config
            .severity
            .insert("CHK002".to_owned(), SeverityLevel::Info);
        let updated = apply_severity_override(candidate(RuleId::Chk002), &config).expect("issue");
        assert_eq!(updated.severity, Severity::Info);
    }
}
