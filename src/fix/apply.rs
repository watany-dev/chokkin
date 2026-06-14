//! Apply optional manifest fixes (pipeline step 13).

use std::path::Path;

use crate::discovery::ProjectRoot;
use crate::manifest::LoadedManifest;
use crate::rules::{IssueReport, RuleId};

use super::containment::resolve_contained_path;
use super::error::FixError;
use super::plan::{FixAction, plan_fixes};
use super::pyproject::{move_group_to_runtime, remove_by_label};
use super::requirements::remove_dependency_line;
use super::setup_cfg::remove_dependency as remove_setup_cfg_dependency;
use super::types::{AppliedFix, FixOptions, FixReport, SkippedFix, SkippedReason};

/// Apply safe automatic fixes for fixable issues in `report`.
///
/// # Errors
///
/// Returns [`FixError`] when a manifest file cannot be read or written.
pub fn apply_fixes(
    report: &IssueReport,
    root: &ProjectRoot,
    manifest: &LoadedManifest,
    options: FixOptions,
) -> Result<FixReport, FixError> {
    let mut report_out = FixReport::default();

    let actions = match plan_fixes(report, manifest, options) {
        Ok(actions) => actions,
        Err(skipped) => {
            report_out.skipped = skipped;
            return Ok(report_out);
        },
    };

    for action in actions {
        match apply_action(root.path.as_path(), &action, options) {
            Ok(applied) => report_out.applied.push(applied),
            Err(error) => report_out.skipped.push(skipped_from_error(&action, &error)),
        }
    }

    if manifest.sources.uv_lock && !report_out.applied.is_empty() {
        report_out
            .reminders
            .push("Run `uv lock` to refresh uv.lock".to_owned());
    }

    Ok(report_out)
}

fn apply_action(
    root: &Path,
    action: &FixAction,
    options: FixOptions,
) -> Result<AppliedFix, FixError> {
    if options.dry_run {
        return Ok(applied_preview(action));
    }

    match action {
        FixAction::RemoveDependency {
            rule,
            name,
            file,
            label,
            line,
        } => {
            let path = resolve_contained_path(root, file)?;
            let description = if std::path::Path::new(file)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
            {
                remove_by_label(&path, label)?
            } else if std::path::Path::new(file)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("cfg"))
            {
                remove_setup_cfg_dependency(&path, name)?
            } else {
                remove_dependency_line(&path, name, *line)?
            };
            Ok(AppliedFix {
                rule: *rule,
                subject: crate::rules::IssueSubject::Distribution { name: name.clone() },
                file: file.clone(),
                description,
            })
        },
        FixAction::MoveToRuntime {
            name,
            file,
            from_label,
            raw,
        } => {
            let path = root.join(file);
            let description = move_group_to_runtime(&path, from_label, raw)?;
            Ok(AppliedFix {
                rule: RuleId::Chk005,
                subject: crate::rules::IssueSubject::Distribution { name: name.clone() },
                file: file.clone(),
                description,
            })
        },
        FixAction::RemoveFile { path: file } => {
            let path = resolve_contained_path(root, file)?;
            std::fs::remove_file(&path).map_err(|source| FixError::Io {
                path: file.clone(),
                source,
            })?;
            Ok(AppliedFix {
                rule: RuleId::Chk001,
                subject: crate::rules::IssueSubject::File { path: file.clone() },
                file: file.clone(),
                description: format!("removed unreachable file `{file}`"),
            })
        },
    }
}

fn applied_preview(action: &FixAction) -> AppliedFix {
    match action {
        FixAction::RemoveDependency {
            rule, name, file, ..
        } => AppliedFix {
            rule: *rule,
            subject: crate::rules::IssueSubject::Distribution { name: name.clone() },
            file: file.clone(),
            description: format!("would remove `{name}` from {file}"),
        },
        FixAction::MoveToRuntime { name, file, .. } => AppliedFix {
            rule: RuleId::Chk005,
            subject: crate::rules::IssueSubject::Distribution { name: name.clone() },
            file: file.clone(),
            description: format!("would move `{name}` to runtime in {file}"),
        },
        FixAction::RemoveFile { path } => AppliedFix {
            rule: RuleId::Chk001,
            subject: crate::rules::IssueSubject::File { path: path.clone() },
            file: path.clone(),
            description: format!("would remove unreachable file `{path}`"),
        },
    }
}

fn skipped_from_error(action: &FixAction, error: &FixError) -> SkippedFix {
    let (rule, subject) = match action {
        FixAction::RemoveDependency { rule, name, .. } => (
            *rule,
            crate::rules::IssueSubject::Distribution { name: name.clone() },
        ),
        FixAction::MoveToRuntime { name, .. } => (
            RuleId::Chk005,
            crate::rules::IssueSubject::Distribution { name: name.clone() },
        ),
        FixAction::RemoveFile { path } => (
            RuleId::Chk001,
            crate::rules::IssueSubject::File { path: path.clone() },
        ),
    };
    SkippedFix {
        rule,
        subject,
        reason: SkippedReason::UnsupportedTarget,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Confidence;
    use crate::discovery::RootMarker;
    use crate::manifest::{
        DeclaredDependency, DependencyContext, DependencyOrigin, LockfileGraph, ManifestSources,
        ProjectMetadata,
    };
    use crate::rules::{Issue, IssueLocation, IssueReport, IssueSummary, Severity};

    fn empty_manifest(root: &ProjectRoot) -> LoadedManifest {
        LoadedManifest {
            root: root.clone(),
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    fn project_root(path: &std::path::Path) -> ProjectRoot {
        ProjectRoot {
            path: path.to_path_buf(),
            marker: RootMarker::PyProjectToml,
            start: path.to_path_buf(),
        }
    }

    fn issue_report(issue: Issue) -> IssueReport {
        IssueReport {
            issues: vec![issue],
            suppressed: Vec::new(),
            summary: IssueSummary::default(),
            exit_status: crate::ExitStatus::IssuesFound,
        }
    }

    fn unused_file_issue(path: &str) -> Issue {
        Issue {
            rule: RuleId::Chk001,
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: "unused".to_owned(),
            workspace_member: None,
            location: IssueLocation {
                file: Some(path.to_owned()),
                line: None,
                manifest: None,
            },
            subject: crate::rules::IssueSubject::File {
                path: path.to_owned(),
            },
            explain: None,
        }
    }

    #[test]
    fn dry_run_does_not_write_files() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("pyproject.toml");
        std::fs::write(
            &path,
            "[project]\nname = \"demo\"\ndependencies = [\"boto3>=1.0\"]\n",
        )
        .expect("write");

        let root = ProjectRoot {
            path: dir.path().to_path_buf(),
            marker: RootMarker::PyProjectToml,
            start: dir.path().to_path_buf(),
        };
        let manifest = LoadedManifest {
            root: root.clone(),
            metadata: ProjectMetadata::default(),
            dependencies: vec![DeclaredDependency {
                name: "boto3".to_owned(),
                extras: Vec::new(),
                marker: None,
                specifier: Some(">=1.0".to_owned()),
                context: DependencyContext::Runtime,
                origin: DependencyOrigin {
                    file: "pyproject.toml".to_owned(),
                    line: None,
                    label: "project.dependencies[0]".to_owned(),
                },
                opaque: false,
            }],
            constraints: Vec::new(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources {
                pyproject_toml: true,
                ..ManifestSources::default()
            },
            warnings: Vec::new(),
        };
        let issue = Issue {
            rule: RuleId::Chk002,
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: "unused".to_owned(),
            workspace_member: None,
            location: IssueLocation {
                file: None,
                line: None,
                manifest: Some(DependencyOrigin {
                    file: "pyproject.toml".to_owned(),
                    line: None,
                    label: "project.dependencies[0]".to_owned(),
                }),
            },
            subject: crate::rules::IssueSubject::Distribution {
                name: "boto3".to_owned(),
            },
            explain: None,
        };
        let report = IssueReport {
            issues: vec![issue],
            suppressed: Vec::new(),
            summary: IssueSummary::default(),
            exit_status: crate::ExitStatus::IssuesFound,
        };

        let fix_report = apply_fixes(
            &report,
            &root,
            &manifest,
            FixOptions {
                dry_run: true,
                ..FixOptions::default()
            },
        )
        .expect("apply");

        assert_eq!(fix_report.applied.len(), 1);
        let contents = std::fs::read_to_string(&path).expect("read");
        assert!(contents.contains("boto3"));
    }

    #[test]
    fn file_removal_requires_allow_flag() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/legacy.py"), "").expect("write");

        let root = project_root(dir.path());
        let manifest = empty_manifest(&root);
        let report = issue_report(unused_file_issue("src/legacy.py"));

        let fix_report =
            apply_fixes(&report, &root, &manifest, FixOptions::default()).expect("apply");

        assert!(dir.path().join("src/legacy.py").exists());
        assert!(fix_report.applied.is_empty());
        assert_eq!(fix_report.skipped.len(), 1);
        assert_eq!(fix_report.skipped[0].reason, SkippedReason::FileRemovalDenied);
    }

    #[test]
    fn file_removal_dry_run_keeps_file() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/legacy.py"), "").expect("write");

        let root = project_root(dir.path());
        let manifest = empty_manifest(&root);
        let report = issue_report(unused_file_issue("src/legacy.py"));

        let fix_report = apply_fixes(
            &report,
            &root,
            &manifest,
            FixOptions {
                dry_run: true,
                allow_remove_files: true,
                ..FixOptions::default()
            },
        )
        .expect("apply");

        assert!(dir.path().join("src/legacy.py").exists());
        assert_eq!(fix_report.applied.len(), 1);
        assert_eq!(fix_report.applied[0].rule, RuleId::Chk001);
    }

    #[test]
    fn file_removal_deletes_unreachable_file() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/legacy.py"), "").expect("write");

        let root = project_root(dir.path());
        let manifest = empty_manifest(&root);
        let report = issue_report(unused_file_issue("src/legacy.py"));

        let fix_report = apply_fixes(
            &report,
            &root,
            &manifest,
            FixOptions {
                allow_remove_files: true,
                ..FixOptions::default()
            },
        )
        .expect("apply");

        assert!(!dir.path().join("src/legacy.py").exists());
        assert_eq!(fix_report.applied.len(), 1);
        assert_eq!(fix_report.applied[0].rule, RuleId::Chk001);
    }
}
