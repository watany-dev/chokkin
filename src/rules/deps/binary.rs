//! CHK008 unlisted binary dependency detection.

use crate::config::Confidence;
use crate::plugins::PluginHints;
use crate::resolver::ResolutionIndex;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};

use super::used::DeclaredIndex;

/// Detect CLI binaries used in config but not declared as dependencies.
pub(super) fn detect_unlisted_binaries(
    declared: &DeclaredIndex<'_>,
    resolution: &ResolutionIndex,
    plugins: &PluginHints,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();
    let mut reported = std::collections::HashSet::new();

    for usage in plugins.all_binary_usages() {
        let Some(distribution) = resolution.binary_resolutions.get(&usage.binary) else {
            continue;
        };
        if declared.contains_key(distribution) {
            continue;
        }
        if !reported.insert(distribution.clone()) {
            continue;
        }

        candidates.push(IssueCandidate {
            rule: RuleId::Chk008,
            subject: IssueSubject::Binary {
                name: usage.binary.clone(),
            },
            severity: Severity::Warning,
            confidence: Confidence::Certain,
            message: format!(
                "binary {} resolves to {distribution} but it is not declared in the manifest",
                usage.binary
            ),
            origins: vec![Origin::Binary(usage.origin.clone())],
            explain: ExplainData {
                summary: format!(
                    "{} requires declared dependency {distribution}",
                    usage.binary
                ),
                details: vec![format!("binary usage in {}", usage.origin.file)],
            },
        });
    }

    candidates
}
