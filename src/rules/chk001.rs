//! CHK001 unused file candidate generation (pipeline step 12).

use crate::config::{Confidence, ProjectMode};
use crate::reachability::UnreachableFile;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, RuleId, Severity};
use crate::sources::is_test_data_path;

/// Build CHK001 candidates from unreachable files.
///
/// Unreachable test data is skipped unless `strict`.
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
        let confidence = file.max_confidence;
        let severity = chk001_severity(file.mode, confidence);

        candidates.push(IssueCandidate {
            rule: RuleId::Chk001,
            subject: IssueSubject::File {
                path: file.path.clone(),
            },
            severity,
            confidence,
            message: format!("file `{}` is not reachable from any entry root", file.path),
            workspace_member: None,
            origins: Vec::new(),
            explain: ExplainData {
                summary: format!("{path} is unreachable from entry roots", path = file.path),
                details: vec!["reason: NotReachable".to_owned()],
            },
        });
    }

    candidates
}

fn chk001_severity(mode: ProjectMode, confidence: Confidence) -> Severity {
    if mode == ProjectMode::Library && confidence == Confidence::Maybe {
        Severity::Warning
    } else {
        Severity::Error
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProjectMode;
    use crate::graph::FileId;

    #[test]
    fn unreachable_test_data_is_reported_only_in_strict_mode() {
        let unreachable = ["tests/data/case.py", "src/legacy.py"].map(|path| UnreachableFile {
            file: FileId(0),
            path: path.to_owned(),
            max_confidence: Confidence::Certain,
            mode: ProjectMode::App,
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
    fn only_library_maybe_is_downgraded_to_warning() {
        let cases = [
            (ProjectMode::App, Confidence::Certain, Severity::Error),
            (ProjectMode::App, Confidence::Likely, Severity::Error),
            (ProjectMode::App, Confidence::Maybe, Severity::Error),
            (ProjectMode::Library, Confidence::Certain, Severity::Error),
            (ProjectMode::Library, Confidence::Likely, Severity::Error),
            (ProjectMode::Library, Confidence::Maybe, Severity::Warning),
        ];
        for (mode, confidence, expected) in cases {
            assert_eq!(
                chk001_severity(mode, confidence),
                expected,
                "{mode:?} x {confidence:?}"
            );
        }
    }
}
