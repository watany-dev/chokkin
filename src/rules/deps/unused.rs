//! YOK002 unused dependency detection.

use crate::config::Confidence;
use crate::manifest::DeclaredDependency;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};

/// Detect declared dependencies with no matching usage.
pub(super) fn detect_unused_dependencies(
    declared: &[&DeclaredDependency],
    used: &indexmap::IndexSet<String>,
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

        let (confidence, severity) = unused_confidence(dep, strict);
        candidates.push(IssueCandidate {
            rule: RuleId::Yok002,
            subject: IssueSubject::Distribution {
                name: dep.name.clone(),
            },
            severity,
            confidence,
            message: format!(
                "declared in {}, no reachable import, config, or binary usage found",
                dep.origin.label
            ),
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

/// Whether a distribution name looks like a types stub package.
#[must_use]
pub(super) fn is_types_stub(name: &str) -> bool {
    name.starts_with("types-") || name.ends_with("-stubs")
}
