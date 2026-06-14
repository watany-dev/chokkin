//! CHK002 unused dependency detection.

use crate::config::{ChokkinConfig, Confidence};
use crate::manifest::DeclaredDependency;
use crate::manifest::normalize_distribution_name;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};

use super::context::{DeclarationBucket, declaration_bucket};

/// Detect declared dependencies with no matching usage.
pub(super) fn detect_unused_dependencies(
    declared: &[&DeclaredDependency],
    used: &indexmap::IndexSet<String>,
    config: &ChokkinConfig,
    strict: bool,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for dep in declared {
        if dep.opaque {
            continue;
        }
        if !seen.insert(dep.name.as_str()) {
            continue;
        }
        if used.contains(&dep.name) {
            continue;
        }
        if should_suppress_unused_report(&dep.context, config, strict) {
            continue;
        }
        if !strict && dep.marker.is_some() {
            continue;
        }

        let (confidence, severity) = unused_confidence(dep, strict);
        candidates.push(IssueCandidate {
            rule: RuleId::Chk002,
            subject: IssueSubject::Distribution {
                name: dep.name.clone(),
            },
            severity,
            confidence,
            message: format!(
                "declared in {}, no reachable import, config, or binary usage found",
                dep.origin.label
            ),
            workspace_member: None,
            origins: vec![Origin::Manifest(dep.origin.clone())],
            explain: ExplainData {
                summary: format!("{} is declared but not used", dep.name),
                details: vec![
                    format!("declaration: {}", dep.origin.label),
                    "no import, plugin module ref, or binary usage resolved to this distribution"
                        .to_owned(),
                ],
            },
        });
    }

    candidates
}

/// Dev, optional-extra, and setup-extra declarations are not reported unless `--strict`.
fn should_suppress_unused_report(
    context: &crate::manifest::DependencyContext,
    config: &ChokkinConfig,
    strict: bool,
) -> bool {
    if strict {
        return false;
    }
    if declaration_bucket(context, &config.dependencies) == DeclarationBucket::Dev {
        return true;
    }
    matches!(
        context,
        crate::manifest::DependencyContext::OptionalExtra(_)
            | crate::manifest::DependencyContext::SetupExtra(_)
    )
}

fn unused_confidence(dep: &DeclaredDependency, strict: bool) -> (Confidence, Severity) {
    if dep.marker.is_some() {
        let severity = if strict {
            Severity::Error
        } else {
            Severity::Warning
        };
        return (Confidence::Likely, severity);
    }
    (Confidence::Certain, Severity::Error)
}

/// Whether a `types-*` stub name looks like a types stub package.
#[must_use]
pub(super) fn is_types_stub(name: &str) -> bool {
    let normalized = normalize_distribution_name(name);
    normalized.starts_with("types-") || normalized.ends_with("-stubs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::manifest::{DeclaredDependency, DependencyContext, DependencyOrigin};

    fn dep(context: DependencyContext) -> DeclaredDependency {
        DeclaredDependency {
            name: "pytest".to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(1),
                label: "dependency-groups.dev[0]".to_owned(),
            },
            opaque: false,
        }
    }

    #[test]
    fn suppresses_unused_dev_group_by_default() {
        let config = default_config();
        let dev_dep = dep(DependencyContext::Group("dev".to_owned()));
        let candidates =
            detect_unused_dependencies(&[&dev_dep], &indexmap::IndexSet::new(), &config, false);
        assert!(candidates.is_empty());
    }

    #[test]
    fn reports_unused_dev_group_in_strict_mode() {
        let config = default_config();
        let dev_dep = dep(DependencyContext::Group("dev".to_owned()));
        let candidates =
            detect_unused_dependencies(&[&dev_dep], &indexmap::IndexSet::new(), &config, true);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].rule, RuleId::Chk002);
    }

    #[test]
    fn reports_unused_runtime_dependency() {
        let config = default_config();
        let runtime_dep = dep(DependencyContext::Runtime);
        let candidates =
            detect_unused_dependencies(&[&runtime_dep], &indexmap::IndexSet::new(), &config, false);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn suppresses_unused_optional_extra_by_default() {
        let config = default_config();
        let optional_dep = dep(DependencyContext::OptionalExtra("brotli".to_owned()));
        let candidates = detect_unused_dependencies(
            &[&optional_dep],
            &indexmap::IndexSet::new(),
            &config,
            false,
        );
        assert!(candidates.is_empty());
    }
}
