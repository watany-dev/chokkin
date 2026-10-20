//! CHK001 unused file candidate generation (pipeline step 12).

use crate::reachability::UnreachableFile;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, RuleId, Severity};
use crate::sources::is_test_data_path;

/// Build CHK001 candidates from unreachable files.
///
/// Unreachable test data is skipped unless `strict`. Every candidate is a
/// warning (spec §3): unreachability from the import graph misses files a
/// framework, a tool or a test loads by name or path, so it does not fail a
/// default run on its own. Library mode lowers confidence, not severity.
#[must_use]
pub(super) fn chk001_candidates(
    unreachable: &[UnreachableFile],
    strict: bool,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for file in unreachable {
        if !strict && is_test_data_path(&file.path) {
            continue;
        }
        candidates.push(IssueCandidate {
            rule: RuleId::Chk001,
            subject: IssueSubject::File {
                path: file.path.clone(),
            },
            severity: Severity::Warning,
            confidence: file.max_confidence,
            message: format!("file `{}` is not reachable from any entry root", file.path),
            workspace_member: None,
            origins: Vec::new(),
            explain: ExplainData {
                summary: format!("{path} is unreachable from entry roots", path = file.path),
                details: std::iter::once("reason: NotReachable".to_owned())
                    .chain(file.mode_reason.clone())
                    .collect(),
            },
        });
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Confidence, ProjectMode};
    use crate::graph::FileId;

    #[test]
    fn unreachable_test_data_is_reported_only_in_strict_mode() {
        let unreachable = ["tests/data/case.py", "src/legacy.py"].map(|path| UnreachableFile {
            file: FileId(0),
            path: path.to_owned(),
            max_confidence: Confidence::Certain,
            mode: ProjectMode::App,
            mode_reason: None,
        });
        let paths = |strict| {
            chk001_candidates(&unreachable, strict)
                .into_iter()
                .map(|candidate| match candidate.subject {
                    IssueSubject::File { path } => path,
                    other => panic!("unexpected subject {other:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(false), ["src/legacy.py"]);
        assert_eq!(paths(true), ["tests/data/case.py", "src/legacy.py"]);
    }

    #[test]
    fn every_candidate_is_a_warning_at_the_file_confidence() {
        let modes = [ProjectMode::App, ProjectMode::Library];
        let confidences = [Confidence::Certain, Confidence::Likely, Confidence::Maybe];
        for mode in modes {
            for confidence in confidences {
                let unreachable = [UnreachableFile {
                    file: FileId(0),
                    path: "src/legacy.py".to_owned(),
                    max_confidence: confidence,
                    mode,
                    mode_reason: None,
                }];
                let candidates = chk001_candidates(&unreachable, false);
                assert_eq!(
                    candidates
                        .iter()
                        .map(|candidate| (candidate.severity, candidate.confidence))
                        .collect::<Vec<_>>(),
                    [(Severity::Warning, confidence)],
                    "{mode:?} x {confidence:?}"
                );
            }
        }
    }
}
