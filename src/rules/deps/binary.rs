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
            workspace_member: None,
            origins: vec![Origin::Binary(usage.origin.clone())],
            explain: ExplainData {
                summary: format!(
                    "{} requires declared dependency {distribution}",
                    usage.binary
                ),
                details: vec![match usage.origin.line {
                    Some(line) => format!("binary usage in {}:{line}", usage.origin.file),
                    None => format!("binary usage in {}", usage.origin.file),
                }],
            },
        });
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{BinaryUsage, ReferenceOrigin};

    fn hints(binaries: &[&str]) -> PluginHints {
        PluginHints {
            contributions: Vec::new(),
            config_binary_usages: binaries
                .iter()
                .map(|binary| BinaryUsage {
                    binary: (*binary).to_owned(),
                    origin: ReferenceOrigin {
                        file: "Makefile".to_owned(),
                        line: Some(1),
                        label: "recipe".to_owned(),
                    },
                })
                .collect(),
            config_used_distributions: Vec::new(),
            config_module_refs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn resolution(pairs: &[(&str, &str)]) -> ResolutionIndex {
        ResolutionIndex {
            binary_resolutions: pairs
                .iter()
                .map(|(binary, distribution)| ((*binary).to_owned(), (*distribution).to_owned()))
                .collect(),
            ..ResolutionIndex::default()
        }
    }

    fn binary_names(candidates: &[IssueCandidate]) -> Vec<&str> {
        candidates
            .iter()
            .filter_map(|candidate| match &candidate.subject {
                IssueSubject::Binary { name } if candidate.rule == RuleId::Chk008 => {
                    Some(name.as_str())
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn undeclared_binary_emits_chk008() {
        let candidates = detect_unlisted_binaries(
            &DeclaredIndex::new(),
            &resolution(&[("pytest", "pytest")]),
            &hints(&["pytest"]),
        );
        assert_eq!(binary_names(&candidates), ["pytest"]);
    }

    #[test]
    fn binaries_of_same_distribution_emit_one_chk008() {
        let candidates = detect_unlisted_binaries(
            &DeclaredIndex::new(),
            &resolution(&[("pytest", "pytest"), ("py.test", "pytest")]),
            &hints(&["pytest", "py.test", "pytest"]),
        );
        assert_eq!(binary_names(&candidates), ["pytest"]);
    }
}
