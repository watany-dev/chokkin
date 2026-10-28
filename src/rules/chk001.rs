//! CHK001 unused file candidate generation (pipeline step 12).

use crate::reachability::UnreachableFile;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, RuleId, Severity};
use crate::sources::{in_examples_dir, in_templates_dir, is_test_data_path};

/// Build CHK001 candidates from unreachable files.
///
/// Unreachable test data is skipped unless `strict`. Every candidate is a
/// warning (spec §3): unreachability from the import graph misses files a
/// framework, a tool or a test loads by name or path, so it does not fail a
/// default run on its own. Library mode lowers confidence, not severity. A
/// file under an `examples/` directory is info: it is a sample users copy or
/// run by hand (weblate's `weblate/examples/check_foo.py`), which nothing
/// imports by design (#1239). So is a test-context file in a directory no
/// reachable file shares, unless it is a conftest or named like a test
/// module: a fixture project, plugin or broken input that a test harness
/// loads by path or directory convention (#1268). A helper beside the test
/// modules that forgot to import it stays a warning. So, too, is a file
/// users select or copy from their own settings, which the tree does not
/// hold (#1258): one under a `templates/` or `*_templates/` directory, a
/// plugin beside the modules a list of module names loads, and one a config
/// file of its package names by a public class or its dotted module path.
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
        let example = in_examples_dir(&file.path);
        let template = !example && in_templates_dir(&file.path);
        let fixture = !example && !template && file.test_fixture;
        // Set only for files none of the above lowers (`apply_user_selected`).
        let named = file.selected_by.as_ref();
        candidates.push(IssueCandidate {
            rule: RuleId::Chk001,
            subject: IssueSubject::File {
                path: file.path.clone(),
            },
            severity: if example || template || fixture || named.is_some() {
                Severity::Info
            } else {
                Severity::Warning
            },
            confidence: file.max_confidence,
            message: format!("file `{}` is not reachable from any entry root", file.path),
            workspace_member: None,
            origins: Vec::new(),
            explain: ExplainData {
                summary: format!("{path} is unreachable from entry roots", path = file.path),
                details: std::iter::once("reason: NotReachable".to_owned())
                    .chain(file.confidence_reason.clone())
                    .chain(example.then(|| "the file is under an `examples/` directory".to_owned()))
                    .chain(template.then(|| {
                        "the file is under a templates directory: a sample users copy".to_owned()
                    }))
                    .chain(fixture.then(|| {
                        "the file is test input: no reachable file shares its directory".to_owned()
                    }))
                    .chain(
                        named.map(|mention| {
                            format!("users select it in their own settings: {mention}")
                        }),
                    )
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
            confidence_reason: None,
            test_fixture: false,
            selected_by: None,
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
    fn files_under_an_examples_dir_are_info() {
        let unreachable = [
            "src/acme/examples/check_foo.py",
            "examples/demo.py",
            "src/acme/examples.py",
            "src/acme/example/demo.py",
        ]
        .map(|path| UnreachableFile {
            file: FileId(0),
            path: path.to_owned(),
            max_confidence: Confidence::Likely,
            mode: ProjectMode::App,
            confidence_reason: None,
            test_fixture: false,
            selected_by: None,
        });
        let severities: Vec<Severity> = chk001_candidates(&unreachable, false)
            .into_iter()
            .map(|candidate| candidate.severity)
            .collect();
        assert_eq!(
            severities,
            [
                Severity::Info,
                Severity::Info,
                Severity::Warning,
                Severity::Warning
            ]
        );
    }

    #[test]
    fn templates_and_files_config_names_are_info() {
        let unreachable = [
            (
                "src/acme/config_templates/default_webserver_config.py",
                None,
            ),
            ("project/templates/settings.py", None),
            ("src/django/template/loader.py", None),
            (
                "src/acme/log/handler.py",
                Some("`src/acme/config.yml:3` names `Handler`"),
            ),
            ("src/acme/log/legacy.py", None),
        ]
        .map(|(path, mention)| UnreachableFile {
            file: FileId(0),
            path: path.to_owned(),
            max_confidence: Confidence::Certain,
            mode: ProjectMode::App,
            confidence_reason: None,
            test_fixture: false,
            selected_by: mention.map(str::to_owned),
        });
        let candidates = chk001_candidates(&unreachable, false);
        let severities: Vec<Severity> = candidates.iter().map(|c| c.severity).collect();
        assert_eq!(
            severities,
            [
                Severity::Info,
                Severity::Info,
                Severity::Warning,
                Severity::Info,
                Severity::Warning
            ]
        );
        assert!(
            candidates[3].explain.details.contains(
                &"users select it in their own settings: `src/acme/config.yml:3` names `Handler`"
                    .to_owned()
            )
        );
    }

    #[test]
    fn test_fixtures_are_info() {
        let unreachable = [
            ("tests/targets/demo/plugin.py", true),
            ("tests/helper.py", false),
        ]
        .map(|(path, test_fixture)| UnreachableFile {
            file: FileId(0),
            path: path.to_owned(),
            max_confidence: Confidence::Certain,
            mode: ProjectMode::App,
            confidence_reason: None,
            test_fixture,
            selected_by: None,
        });
        let severities: Vec<Severity> = chk001_candidates(&unreachable, false)
            .into_iter()
            .map(|candidate| candidate.severity)
            .collect();
        assert_eq!(severities, [Severity::Info, Severity::Warning]);
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
                    confidence_reason: None,
                    test_fixture: false,
                    selected_by: None,
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
