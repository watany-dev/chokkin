//! Confidence and rule-set filters for issue emission.

use crate::config::{ChokkinConfig, Confidence, RuntimeOverrides};
use crate::rules::types::{Issue, RuleId};

/// Effective confidence floor after config and overrides.
#[must_use]
pub fn effective_confidence_floor(
    config: &ChokkinConfig,
    overrides: &RuntimeOverrides,
    strict: bool,
) -> Confidence {
    if strict {
        return Confidence::Maybe;
    }
    overrides.confidence_floor.unwrap_or(config.confidence)
}

/// Returns true when an issue should be shown after confidence filtering.
#[must_use]
pub fn passes_confidence_filter(issue: &Issue, floor: Confidence) -> bool {
    issue.confidence.meets_floor(floor)
}

/// Returns true when an issue passes include/exclude rule filters.
#[must_use]
pub fn passes_rule_filter(issue: &Issue, overrides: &RuntimeOverrides) -> bool {
    if let Some(include) = &overrides.include_rules
        && !include
            .iter()
            .any(|code| RuleId::parse_code(code) == Some(issue.rule))
    {
        return false;
    }
    if let Some(exclude) = &overrides.exclude_rules
        && exclude
            .iter()
            .any(|code| RuleId::parse_code(code) == Some(issue.rule))
    {
        return false;
    }
    true
}

/// Whether an issue should trigger a non-zero exit code.
#[must_use]
pub fn counts_toward_exit(issue: &Issue, strict: bool) -> bool {
    let (min_severity, min_confidence) = if strict {
        (crate::rules::types::Severity::Warning, Confidence::Maybe)
    } else {
        (crate::rules::types::Severity::Error, Confidence::Likely)
    };
    issue.severity >= min_severity && issue.confidence.meets_floor(min_confidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::DependencyOrigin;
    use crate::rules::types::{Issue, IssueLocation, IssueSubject, Severity};

    fn sample_issue(rule: RuleId, confidence: Confidence, severity: Severity) -> Issue {
        Issue {
            rule,
            severity,
            confidence,
            message: "test".to_owned(),
            workspace_member: None,
            location: IssueLocation {
                file: None,
                line: None,
                manifest: Some(DependencyOrigin {
                    file: "pyproject.toml".to_owned(),
                    line: Some(1),
                    label: "project.dependencies[0]".to_owned(),
                }),
            },
            subject: IssueSubject::Distribution {
                name: "pkg".to_owned(),
            },
            explain: None,
        }
    }

    #[test]
    fn likely_issue_hidden_when_floor_is_certain() {
        let issue = sample_issue(RuleId::Chk002, Confidence::Likely, Severity::Error);
        assert!(!passes_confidence_filter(&issue, Confidence::Certain));
    }

    #[test]
    fn strict_exit_counts_warnings() {
        let issue = sample_issue(RuleId::Chk002, Confidence::Maybe, Severity::Warning);
        assert!(counts_toward_exit(&issue, true));
        assert!(!counts_toward_exit(&issue, false));
    }

    #[test]
    fn include_filter_limits_rules() {
        let issue = sample_issue(RuleId::Chk002, Confidence::Certain, Severity::Error);
        let overrides = RuntimeOverrides {
            include_rules: Some(vec!["CHK003".to_owned()]),
            ..RuntimeOverrides::default()
        };
        assert!(!passes_rule_filter(&issue, &overrides));
    }
}
