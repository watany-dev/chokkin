//! CHK001 unused file candidate generation (pipeline step 12).

use crate::config::{Confidence, ProjectMode};
use crate::reachability::UnreachableFile;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, RuleId, Severity};

/// Build CHK001 candidates from unreachable files.
#[must_use]
pub fn chk001_candidates(unreachable: &[UnreachableFile]) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for file in unreachable {
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
