//! Apply optional manifest fixes (pipeline step 13).

use std::path::Path;

use crate::discovery::ProjectRoot;
use crate::manifest::{LoadedManifest, LockfileKind};
use crate::rules::IssueReport;

use super::containment::resolve_contained_path;
use super::error::FixError;
use super::plan::{FixAction, plan_fixes};
use super::pyproject::{add_runtime_dependency, move_group_to_runtime, remove_by_label};
use super::requirements::remove_dependency_line;
use super::setup_cfg::remove_dependency as remove_setup_cfg_dependency;
use super::types::{
    AppliedFix, FixOptions, FixReport, SkippedFix, SkippedReason, WorkspaceFixManifest,
};

/// Apply safe automatic fixes with workspace member manifest context.
pub fn apply_fixes_with_workspace(
    report: &IssueReport,
    root: &ProjectRoot,
    manifest: &LoadedManifest,
    workspace_manifests: &[WorkspaceFixManifest<'_>],
    options: FixOptions,
) -> FixReport {
    let mut report_out = FixReport::default();

    let actions = match plan_fixes(report, manifest, workspace_manifests, options) {
        Ok(actions) => actions,
        Err(skipped) => {
            report_out.skipped = skipped;
            return report_out;
        },
    };

    for action in actions {
        match apply_action(root.path.as_path(), &action, options) {
            Ok(applied) => report_out.applied.push(applied),
            Err(error) => {
                let (rule, subject) = action.rule_subject();
                report_out.skipped.push(SkippedFix {
                    rule,
                    subject,
                    reason: SkippedReason::UnsupportedTarget,
                    detail: error.to_string(),
                });
            },
        }
    }

    if !report_out.applied.is_empty() {
        report_out.reminders = lockfile_reminders(manifest);
    }

    report_out
}

/// chokkin never edits lockfiles (§13), so point at the tool that owns it.
fn lockfile_reminders(manifest: &LoadedManifest) -> Vec<String> {
    let mut reminders = Vec::new();
    let kind = manifest.sources.lockfile.as_ref().map(|source| source.kind);
    if kind == Some(LockfileKind::Uv) {
        reminders.push("Run `uv lock` to refresh uv.lock".to_owned());
    }
    if manifest.sources.poetry || kind == Some(LockfileKind::Poetry) {
        reminders.push("Run `poetry lock` to refresh poetry.lock".to_owned());
    }
    if kind == Some(LockfileKind::Pdm) {
        reminders.push("Run `pdm lock` to refresh pdm.lock".to_owned());
    }
    reminders
}

fn apply_action(
    root: &Path,
    action: &FixAction,
    options: FixOptions,
) -> Result<AppliedFix, FixError> {
    let (file, description) = if options.dry_run {
        preview(action)
    } else {
        perform(root, action)?
    };
    let (rule, subject) = action.rule_subject();
    Ok(AppliedFix {
        rule,
        subject,
        file: file.to_owned(),
        description,
    })
}

/// Returns the edited file and a description of the change.
fn perform<'a>(root: &Path, action: &'a FixAction) -> Result<(&'a str, String), FixError> {
    match action {
        FixAction::RemoveDependency {
            name,
            file,
            label,
            line,
            ..
        } => {
            let path = resolve_contained_path(root, file)?;
            let extension = Path::new(file).extension();
            let description = if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("toml")) {
                remove_by_label(&path, label)?
            } else if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("cfg")) {
                remove_setup_cfg_dependency(&path, name)?
            } else {
                remove_dependency_line(&path, name, *line)?
            };
            Ok((file.as_str(), description))
        },
        FixAction::MoveToRuntime {
            file,
            from_label,
            raw,
            ..
        } => {
            let path = resolve_contained_path(root, file)?;
            Ok((
                file.as_str(),
                move_group_to_runtime(&path, from_label, raw)?,
            ))
        },
        FixAction::AddMissingDependency { name, file } => {
            let path = resolve_contained_path(root, file)?;
            Ok((file.as_str(), add_runtime_dependency(&path, name)?))
        },
        FixAction::RemoveFile { path: file } => {
            let path = resolve_contained_path(root, file)?;
            std::fs::remove_file(&path).map_err(|source| FixError::Io {
                path: file.clone(),
                source,
            })?;
            Ok((file.as_str(), format!("removed unreachable file `{file}`")))
        },
    }
}

/// Dry-run counterpart of `perform`.
fn preview(action: &FixAction) -> (&str, String) {
    match action {
        FixAction::RemoveDependency { name, file, .. } => {
            (file.as_str(), format!("would remove `{name}` from {file}"))
        },
        FixAction::MoveToRuntime { name, file, .. } => (
            file.as_str(),
            format!("would move `{name}` to runtime in {file}"),
        ),
        FixAction::AddMissingDependency { name, file } => {
            (file.as_str(), format!("would add `{name}` to {file}"))
        },
        FixAction::RemoveFile { path } => (
            path.as_str(),
            format!("would remove unreachable file `{path}`"),
        ),
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
    use crate::rules::{
        ExplainData, Issue, IssueLocation, IssueReport, IssueSummary, RuleId, Severity,
    };

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

    fn unused_dependency_issue(name: &str) -> Issue {
        Issue {
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
                name: name.to_owned(),
            },
            explain: None,
        }
    }

    fn missing_dependency_issue(distribution: &str) -> Issue {
        Issue {
            rule: RuleId::Chk003,
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: "missing".to_owned(),
            workspace_member: None,
            location: IssueLocation {
                file: Some("src/app.py".to_owned()),
                line: Some(1),
                manifest: None,
            },
            subject: crate::rules::IssueSubject::Import {
                module: "yaml".to_owned(),
                file: "src/app.py".to_owned(),
                line: 1,
            },
            explain: Some(ExplainData {
                summary: format!("{distribution} is imported but not declared"),
                details: Vec::new(),
            }),
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
                included_via: Vec::new(),
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

        let fix_report = apply_fixes_with_workspace(
            &report,
            &root,
            &manifest,
            &[],
            FixOptions {
                dry_run: true,
                ..FixOptions::default()
            },
        );

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
            apply_fixes_with_workspace(&report, &root, &manifest, &[], FixOptions::default());

        assert!(dir.path().join("src/legacy.py").exists());
        assert!(fix_report.applied.is_empty());
        assert_eq!(fix_report.skipped.len(), 1);
        assert_eq!(
            fix_report.skipped[0].reason,
            SkippedReason::FileRemovalDenied
        );
    }

    #[test]
    fn file_removal_dry_run_keeps_file() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/legacy.py"), "").expect("write");

        let root = project_root(dir.path());
        let manifest = empty_manifest(&root);
        let report = issue_report(unused_file_issue("src/legacy.py"));

        let fix_report = apply_fixes_with_workspace(
            &report,
            &root,
            &manifest,
            &[],
            FixOptions {
                dry_run: true,
                allow_remove_files: true,
                ..FixOptions::default()
            },
        );

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

        let fix_report = apply_fixes_with_workspace(
            &report,
            &root,
            &manifest,
            &[],
            FixOptions {
                allow_remove_files: true,
                ..FixOptions::default()
            },
        );

        assert!(!dir.path().join("src/legacy.py").exists());
        assert_eq!(fix_report.applied.len(), 1);
        assert_eq!(fix_report.applied[0].rule, RuleId::Chk001);
    }

    #[test]
    fn poetry_manifest_fix_reminds_to_refresh_lockfile() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("pyproject.toml");
        std::fs::write(
            &path,
            "[project]\nname = \"demo\"\ndependencies = [\"boto3>=1.0\"]\n",
        )
        .expect("write");

        let root = project_root(dir.path());
        let mut manifest = empty_manifest(&root);
        manifest.dependencies.push(DeclaredDependency {
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
            included_via: Vec::new(),
        });
        manifest.sources.pyproject_toml = true;
        manifest.sources.poetry = true;
        let report = issue_report(unused_dependency_issue("boto3"));

        let fix_report = apply_fixes_with_workspace(
            &report,
            &root,
            &manifest,
            &[],
            FixOptions {
                dry_run: true,
                ..FixOptions::default()
            },
        );

        assert!(
            fix_report
                .reminders
                .iter()
                .any(|reminder| { reminder.contains("poetry lock") })
        );
    }

    #[test]
    fn lockfile_reminders_follow_lockfile_kind() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let root = project_root(dir.path());
        let mut manifest = empty_manifest(&root);
        assert!(lockfile_reminders(&manifest).is_empty());

        manifest.sources.lockfile = Some(crate::manifest::LockfileSource {
            kind: LockfileKind::Pdm,
            path: "pdm.lock".to_owned(),
        });
        assert_eq!(
            lockfile_reminders(&manifest),
            vec!["Run `pdm lock` to refresh pdm.lock".to_owned()]
        );

        manifest.sources.poetry = true;
        manifest.sources.lockfile = Some(crate::manifest::LockfileSource {
            kind: LockfileKind::Poetry,
            path: "poetry.lock".to_owned(),
        });
        assert_eq!(
            lockfile_reminders(&manifest),
            vec!["Run `poetry lock` to refresh poetry.lock".to_owned()]
        );
    }

    #[test]
    fn add_missing_dependency_updates_pyproject() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("pyproject.toml");
        std::fs::write(&path, "[project]\nname = \"demo\"\n").expect("write");

        let root = project_root(dir.path());
        let mut manifest = empty_manifest(&root);
        manifest.sources.pyproject_toml = true;
        let report = issue_report(missing_dependency_issue("pyyaml"));

        let fix_report = apply_fixes_with_workspace(
            &report,
            &root,
            &manifest,
            &[],
            FixOptions {
                add_missing: true,
                ..FixOptions::default()
            },
        );

        assert_eq!(fix_report.applied.len(), 1);
        let updated = std::fs::read_to_string(&path).expect("read");
        assert!(updated.contains("dependencies = [\"pyyaml\"]"));
    }

    #[test]
    fn add_missing_dependency_updates_workspace_member_pyproject() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let member_dir = dir.path().join("services/api");
        std::fs::create_dir_all(&member_dir).expect("mkdir member");
        let member_pyproject = member_dir.join("pyproject.toml");
        std::fs::write(&member_pyproject, "[project]\nname = \"api\"\n").expect("write member");

        let root = project_root(dir.path());
        let root_manifest = empty_manifest(&root);
        let member_root = ProjectRoot {
            path: member_dir,
            marker: RootMarker::PyProjectToml,
            start: dir.path().to_path_buf(),
        };
        let mut member_manifest = empty_manifest(&member_root);
        member_manifest.sources.pyproject_toml = true;
        let workspace_manifest = WorkspaceFixManifest {
            id: "api",
            path: "services/api",
            pyproject_toml: Some("services/api/pyproject.toml"),
            manifest: &member_manifest,
        };
        let mut issue = missing_dependency_issue("pyyaml");
        issue.workspace_member = Some("api".to_owned());
        let report = issue_report(issue);

        let fix_report = apply_fixes_with_workspace(
            &report,
            &root,
            &root_manifest,
            &[workspace_manifest],
            FixOptions {
                add_missing: true,
                ..FixOptions::default()
            },
        );

        assert_eq!(fix_report.applied.len(), 1);
        assert_eq!(fix_report.applied[0].file, "services/api/pyproject.toml");
        let updated = std::fs::read_to_string(&member_pyproject).expect("read member");
        assert!(updated.contains("dependencies = [\"pyyaml\"]"));
    }

    #[test]
    fn move_to_runtime_rejects_escaped_manifest_path() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let action = FixAction::MoveToRuntime {
            name: "pytest".to_owned(),
            file: "../outside.toml".to_owned(),
            from_label: "dependency-groups.dev[0]".to_owned(),
            raw: "pytest".to_owned(),
        };

        let error = apply_action(dir.path(), &action, FixOptions::default())
            .expect_err("escaped path should be rejected");

        assert!(matches!(error, FixError::Unsupported { .. }));
        assert!(error.to_string().contains("escapes the project root"));
    }
}
