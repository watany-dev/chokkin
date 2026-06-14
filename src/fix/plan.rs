//! Map issues to concrete manifest edit operations.

use crate::config::Confidence;
use crate::manifest::{DeclaredDependency, LoadedManifest};
use crate::rules::{Issue, IssueReport, IssueSubject, RuleId};

use super::types::{FixOptions, SkippedFix, SkippedReason};

/// One planned manifest edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FixAction {
    /// Remove a dependency declaration.
    RemoveDependency {
        /// Rule that triggered the removal.
        rule: RuleId,
        /// Distribution name.
        name: String,
        /// Manifest file path.
        file: String,
        /// TOML label or requirements line hint.
        label: String,
        /// 1-based line for requirements files.
        line: Option<u32>,
    },
    /// Move a dependency from a dev group to runtime dependencies.
    MoveToRuntime {
        /// Distribution name.
        name: String,
        /// Manifest file path.
        file: String,
        /// Source label to remove from.
        from_label: String,
        /// PEP 508 requirement string to add.
        raw: String,
    },
    /// Remove an unreachable project file.
    RemoveFile {
        /// Root-relative file path.
        path: String,
    },
}

/// Build fix actions from an issue report.
pub(super) fn plan_fixes(
    report: &IssueReport,
    manifest: &LoadedManifest,
    options: FixOptions,
) -> Result<Vec<FixAction>, Vec<SkippedFix>> {
    if options.add_missing {
        return Err(vec![SkippedFix {
            rule: RuleId::Chk003,
            subject: IssueSubject::Distribution {
                name: String::new(),
            },
            reason: SkippedReason::NotFixable,
            detail: "--add-missing is not implemented in v0.1".to_owned(),
        }]);
    }

    let mut actions = Vec::new();
    let mut skipped = Vec::new();

    for issue in &report.issues {
        match plan_issue_fix(issue, manifest) {
            Ok(Some(action)) => actions.push(action),
            Ok(None) => {},
            Err(skip) => skipped.push(skip),
        }
    }

    if !skipped.is_empty() && actions.is_empty() {
        return Err(skipped);
    }

    Ok(actions)
}

fn plan_issue_fix(
    issue: &Issue,
    manifest: &LoadedManifest,
) -> Result<Option<FixAction>, SkippedFix> {
    match issue.rule {
        RuleId::Chk001 => plan_remove_file(issue, options),
        RuleId::Chk002 if issue.confidence == Confidence::Certain => plan_remove_dependency(issue),
        RuleId::Chk009 if issue.confidence == Confidence::Certain => {
            plan_remove_duplicate(issue, manifest)
        },
        RuleId::Chk005 if issue.confidence == Confidence::Certain => {
            plan_move_to_runtime(issue, manifest)
        },
        RuleId::Chk002 | RuleId::Chk005 | RuleId::Chk009 => Err(skipped(
            issue,
            SkippedReason::NotFixable,
            "only Certain-confidence dependency issues are auto-fixable",
        )),
        _ => Ok(None),
    }
}

fn plan_remove_file(
    issue: &Issue,
    options: FixOptions,
) -> Result<Option<FixAction>, SkippedFix> {
    let IssueSubject::File { path } = &issue.subject else {
        return Ok(None);
    };
    if !options.allow_remove_files {
        return Err(skipped(
            issue,
            SkippedReason::FileRemovalDenied,
            "file removal requires `--allow-remove-files`",
        ));
    }
    if issue.confidence != Confidence::Certain {
        return Err(skipped(
            issue,
            SkippedReason::NotFixable,
            "only Certain-confidence unreachable files are auto-removable",
        ));
    }
    Ok(Some(FixAction::RemoveFile { path: path.clone() }))
}

fn plan_remove_dependency(issue: &Issue) -> Result<Option<FixAction>, SkippedFix> {
    let IssueSubject::Distribution { name } = &issue.subject else {
        return Ok(None);
    };
    let origin = issue.location.manifest.clone().ok_or_else(|| {
        skipped(
            issue,
            SkippedReason::MissingOrigin,
            "missing manifest origin",
        )
    })?;
    Ok(Some(FixAction::RemoveDependency {
        rule: issue.rule,
        name: name.clone(),
        file: origin.file,
        label: origin.label,
        line: origin.line,
    }))
}

fn plan_remove_duplicate(
    issue: &Issue,
    manifest: &LoadedManifest,
) -> Result<Option<FixAction>, SkippedFix> {
    let IssueSubject::Distribution { name } = &issue.subject else {
        return Ok(None);
    };
    let declarations: Vec<&DeclaredDependency> = manifest
        .dependencies
        .iter()
        .filter(|dep| dep.name == *name && !dep.opaque)
        .collect();
    if declarations.len() < 2 {
        return Err(skipped(
            issue,
            SkippedReason::Ambiguous,
            "expected multiple declarations for duplicate fix",
        ));
    }

    let to_remove = declarations
        .iter()
        .max_by_key(|dep| removal_priority(&dep.context))
        .copied()
        .ok_or_else(|| skipped(issue, SkippedReason::Ambiguous, "no removable duplicate"))?;

    Ok(Some(FixAction::RemoveDependency {
        rule: issue.rule,
        name: name.clone(),
        file: to_remove.origin.file.clone(),
        label: to_remove.origin.label.clone(),
        line: to_remove.origin.line,
    }))
}

fn plan_move_to_runtime(
    issue: &Issue,
    manifest: &LoadedManifest,
) -> Result<Option<FixAction>, SkippedFix> {
    let IssueSubject::Distribution { name } = &issue.subject else {
        return Ok(None);
    };
    let declarations: Vec<&DeclaredDependency> = manifest
        .dependencies
        .iter()
        .filter(|dep| dep.name == *name && !dep.opaque)
        .collect();

    let has_runtime = declarations.iter().any(|dep| {
        matches!(
            dep.context,
            crate::manifest::DependencyContext::Runtime
                | crate::manifest::DependencyContext::OptionalExtra(_)
        )
    });
    if has_runtime {
        return Err(skipped(
            issue,
            SkippedReason::Ambiguous,
            "runtime declaration already exists",
        ));
    }

    let dev_only: Vec<_> = declarations
        .iter()
        .filter(|dep| matches!(dep.context, crate::manifest::DependencyContext::Group(_)))
        .collect();
    if dev_only.len() != 1 {
        return Err(skipped(
            issue,
            SkippedReason::Ambiguous,
            "only a single dev-group declaration can be moved automatically",
        ));
    }

    let source = dev_only[0];
    let raw = rebuild_requirement_string(source);

    Ok(Some(FixAction::MoveToRuntime {
        name: name.clone(),
        file: source.origin.file.clone(),
        from_label: source.origin.label.clone(),
        raw,
    }))
}

fn removal_priority(context: &crate::manifest::DependencyContext) -> u8 {
    match context {
        crate::manifest::DependencyContext::Runtime => 0,
        crate::manifest::DependencyContext::OptionalExtra(_) => 1,
        crate::manifest::DependencyContext::Group(_) => 2,
        crate::manifest::DependencyContext::SetupExtra(_) => 3,
    }
}

fn rebuild_requirement_string(dep: &DeclaredDependency) -> String {
    let mut raw = dep.name.clone();
    if let Some(spec) = &dep.specifier {
        raw.push_str(spec);
    }
    if let Some(marker) = &dep.marker {
        raw.push_str(" ; ");
        raw.push_str(marker);
    }
    raw
}

fn skipped(issue: &Issue, reason: SkippedReason, detail: &str) -> SkippedFix {
    SkippedFix {
        rule: issue.rule,
        subject: issue.subject.clone(),
        reason,
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::manifest::{
        DependencyContext, DependencyOrigin, LoadedManifest, LockfileGraph, ManifestSources,
        ProjectMetadata,
    };
    use crate::rules::{Issue, IssueLocation, IssueReport, IssueSummary, Severity};

    fn manifest_with(deps: Vec<DeclaredDependency>) -> LoadedManifest {
        LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            metadata: ProjectMetadata::default(),
            dependencies: deps,
            constraints: Vec::new(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn plans_chk002_removal_for_certain_issue() {
        let manifest = manifest_with(vec![DeclaredDependency {
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
        }]);
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
            subject: IssueSubject::Distribution {
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
        let actions = plan_fixes(&report, &manifest, FixOptions::default()).expect("plan");
        assert_eq!(actions.len(), 1);
    }
}
