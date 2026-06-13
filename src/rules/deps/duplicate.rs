//! YOK009 duplicate dependency declaration detection.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::{Confidence, YokeiConfig};
use crate::manifest::DeclaredDependency;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};

use super::context::{DeclarationBucket, declaration_bucket};

/// Detect the same distribution declared in multiple incompatible contexts.
pub(super) fn detect_duplicate_dependencies(
    manifest_deps: &[DeclaredDependency],
    config: &YokeiConfig,
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
            rule: RuleId::Yok009,
            subject: IssueSubject::Distribution {
                name: name.to_owned(),
            },
            severity: Severity::Warning,
            confidence: Confidence::Certain,
            message: format!(
                "{name} is declared in multiple contexts: {}",
                labels.join(", ")
            ),
            origins,
            explain: ExplainData {
                summary: format!("{name} has duplicate declarations"),
                details: labels,
            },
        });
    }

    candidates
}
