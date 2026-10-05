//! CHK009 duplicate dependency declaration detection.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::{ChokkinConfig, Confidence};
use crate::manifest::DeclaredDependency;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};

use super::context::{DeclarationBucket, declaration_bucket};

/// Detect the same distribution declared in multiple incompatible contexts.
pub(super) fn detect_duplicate_dependencies(
    manifest_deps: &[DeclaredDependency],
    config: &ChokkinConfig,
) -> Vec<IssueCandidate> {
    let mut by_name: BTreeMap<&str, BTreeSet<DeclarationBucket>> = BTreeMap::new();
    let mut origins_by_name: BTreeMap<&str, Vec<&DeclaredDependency>> = BTreeMap::new();

    for dep in manifest_deps {
        if dep.opaque {
            continue;
        }
        by_name
            .entry(dep.name.as_str())
            .or_default()
            .insert(declaration_bucket(&dep.context, &config.dependencies));
        origins_by_name
            .entry(dep.name.as_str())
            .or_default()
            .push(dep);
    }

    let mut candidates = Vec::new();
    for (name, buckets) in by_name {
        if buckets.len() <= 1 {
            continue;
        }
        let labels: Vec<String> = buckets.iter().map(DeclarationBucket::label).collect();
        let origins: Vec<Origin> = origins_by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|dep| Origin::Manifest(dep.origin.clone()))
            .collect();

        candidates.push(IssueCandidate {
            rule: RuleId::Chk009,
            subject: IssueSubject::Distribution {
                name: name.to_owned(),
            },
            severity: Severity::Warning,
            confidence: Confidence::Certain,
            message: format!(
                "{name} is declared in multiple contexts: {}",
                labels.join(", ")
            ),
            workspace_member: None,
            origins,
            explain: ExplainData {
                summary: format!("{name} has duplicate declarations"),
                details: labels,
            },
        });
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::manifest::{DependencyContext, DependencyOrigin};

    fn dep(name: &str, context: DependencyContext, opaque: bool) -> DeclaredDependency {
        DeclaredDependency {
            name: name.to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(1),
                label: name.to_owned(),
            },
            opaque,
            included_via: Vec::new(),
        }
    }

    #[test]
    fn reports_distribution_declared_in_distinct_buckets() {
        let deps = [
            dep("requests", DependencyContext::Runtime, false),
            dep(
                "requests",
                DependencyContext::Group("dev".to_owned()),
                false,
            ),
            dep(
                "requests",
                DependencyContext::Group("typing".to_owned()),
                false,
            ),
            dep(
                "requests",
                DependencyContext::OptionalExtra("http".to_owned()),
                false,
            ),
        ];
        let found = detect_duplicate_dependencies(&deps, &default_config());
        assert_eq!(found.len(), 1);
        let issue = &found[0];
        assert_eq!(issue.rule, RuleId::Chk009);
        assert_eq!(
            issue.subject,
            IssueSubject::Distribution {
                name: "requests".to_owned()
            }
        );
        assert_eq!(
            issue.message,
            "requests is declared in multiple contexts: runtime, dev, type, optional:http"
        );
        assert_eq!(issue.origins.len(), 4);
    }

    #[test]
    fn same_bucket_and_opaque_declarations_are_not_duplicates() {
        let deps = [
            dep("requests", DependencyContext::Runtime, false),
            dep("requests", DependencyContext::Runtime, false),
            dep("pytest", DependencyContext::Group("dev".to_owned()), false),
            dep("pytest", DependencyContext::Group("test".to_owned()), false),
            dep("opaque", DependencyContext::Runtime, false),
            dep("opaque", DependencyContext::Group("dev".to_owned()), true),
        ];
        assert_eq!(detect_duplicate_dependencies(&deps, &default_config()), []);
    }
}
