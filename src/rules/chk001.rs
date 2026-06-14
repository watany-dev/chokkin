//! CHK001 unused file candidate generation (pipeline step 12).

use crate::config::Confidence;
use crate::entry::ResolvedMode;
use crate::reachability::{UnreachableFile, UnreachableReason};
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, RuleId, Severity};

/// Build CHK001 candidates from unreachable files.
#[must_use]
pub fn chk001_candidates(
    unreachable: &[UnreachableFile],
    mode: &ResolvedMode,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for file in unreachable {
        if !is_chk001_candidate(file) {
            continue;
        }

        let confidence = file.max_confidence;
        let severity = chk001_severity(mode.mode, confidence);

        candidates.push(IssueCandidate {
            rule: RuleId::Chk001,
            subject: IssueSubject::File {
                path: file.path.clone(),
            },
            severity,
            confidence,
            message: format!("file `{}` is not reachable from any entry root", file.path),
            origins: Vec::new(),
            explain: ExplainData {
                summary: format!("{path} is unreachable from entry roots", path = file.path),
                details: file
                    .reasons
                    .iter()
                    .map(|reason| format!("reason: {reason:?}"))
                    .collect(),
            },
        });
    }

    candidates
}

fn is_chk001_candidate(file: &UnreachableFile) -> bool {
    file.reasons
        .iter()
        .any(|reason| matches!(reason, UnreachableReason::NotReachable))
}

fn chk001_severity(mode: crate::config::ProjectMode, confidence: Confidence) -> Severity {
    if mode == crate::config::ProjectMode::Library && confidence == Confidence::Maybe {
        Severity::Warning
    } else {
        Severity::Error
    }
}
