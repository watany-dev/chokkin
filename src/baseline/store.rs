//! Baseline read, filter, and atomic write operations.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::VERSION;
use crate::rules::{
    Issue, IssueReport, IssueSummary, SuppressReason, SuppressedIssue, issue_fingerprint,
    issue_stable_target,
};

use super::types::{BaselineEntry, BaselineError, BaselineFile, BaselineReport};

/// Apply a baseline file by suppressing matching issues.
pub fn apply_baseline(
    report: &mut IssueReport,
    root: &Path,
    baseline_path: &Path,
) -> Result<BaselineReport, BaselineError> {
    let path = resolve_baseline_path(root, baseline_path)?;
    if !path.exists() {
        return Ok(BaselineReport {
            path: Some(display_path(root, &path)),
            ..BaselineReport::default()
        });
    }

    let baseline = read_baseline(&path)?;
    let fingerprints: BTreeSet<_> = baseline
        .issues
        .iter()
        .map(|entry| entry.fingerprint.as_str())
        .collect();

    let mut kept = Vec::new();
    let mut suppressed_count = 0_u32;
    for issue in report.issues.drain(..) {
        let fingerprint = issue_fingerprint(&issue);
        if fingerprints.contains(fingerprint.as_str()) {
            suppressed_count = suppressed_count.saturating_add(1);
            report.suppressed.push(SuppressedIssue {
                issue,
                reason: SuppressReason::Baseline,
            });
        } else {
            kept.push(issue);
        }
    }

    report.issues = kept;
    report.summary = build_summary(&report.issues);
    report.exit_status = compute_exit_status(&report.issues, report.exit_status);

    Ok(BaselineReport {
        path: Some(display_path(root, &path)),
        suppressed: suppressed_count,
        written: 0,
    })
}

/// Write the current issue set as a baseline file.
pub fn write_baseline(
    report: &IssueReport,
    root: &Path,
    baseline_path: &Path,
) -> Result<BaselineReport, BaselineError> {
    let path = resolve_baseline_path(root, baseline_path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| BaselineError::Io {
            path: parent.display().to_string(),
            source,
        })?;
        ensure_parent_inside_root(root, parent)?;
    }
    let issues = report
        .issues
        .iter()
        .map(|issue| BaselineEntry {
            fingerprint: issue_fingerprint(issue),
            code: issue.rule.as_code().to_owned(),
            target: issue_stable_target(issue),
        })
        .collect::<Vec<_>>();
    let written = u32::try_from(issues.len()).unwrap_or(u32::MAX);
    let file = BaselineFile {
        chokkin_version: VERSION.to_owned(),
        generated_at: generated_at(),
        issues,
    };
    let contents = serde_json::to_string_pretty(&file).map_err(|source| BaselineError::Json {
        path: display_path(root, &path),
        detail: source.to_string(),
    })?;
    atomic_write(&path, &format!("{contents}\n"))?;
    Ok(BaselineReport {
        path: Some(display_path(root, &path)),
        suppressed: 0,
        written,
    })
}

fn read_baseline(path: &Path) -> Result<BaselineFile, BaselineError> {
    let contents = fs::read_to_string(path).map_err(|source| BaselineError::Io {
        path: path.display().to_string(),
        source,
    })?;
    serde_json::from_str(&contents).map_err(|source| BaselineError::Json {
        path: path.display().to_string(),
        detail: source.to_string(),
    })
}

fn resolve_baseline_path(root: &Path, baseline_path: &Path) -> Result<PathBuf, BaselineError> {
    let root = root.canonicalize().map_err(|source| BaselineError::Io {
        path: root.display().to_string(),
        source,
    })?;
    let path = if baseline_path.is_absolute() {
        baseline_path.to_path_buf()
    } else {
        root.join(baseline_path)
    };
    let resolved = resolve_through_existing_ancestor(&path)?;
    if !resolved.starts_with(&root) {
        return Err(BaselineError::OutsideRoot {
            path: path.display().to_string(),
        });
    }
    Ok(resolved)
}

fn resolve_through_existing_ancestor(path: &Path) -> Result<PathBuf, BaselineError> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            break;
        };
        missing.push(name.to_owned());
        let Some(parent) = ancestor.parent() else {
            break;
        };
        ancestor = parent;
    }

    let mut resolved = ancestor
        .canonicalize()
        .map_err(|source| BaselineError::Io {
            path: path.display().to_string(),
            source,
        })?;
    for component in missing.iter().rev() {
        if !matches!(
            Path::new(component).components().next(),
            Some(Component::Normal(_))
        ) {
            return Err(BaselineError::OutsideRoot {
                path: path.display().to_string(),
            });
        }
        resolved.push(component);
    }
    Ok(resolved)
}

fn ensure_parent_inside_root(root: &Path, parent: &Path) -> Result<(), BaselineError> {
    let root = root.canonicalize().map_err(|source| BaselineError::Io {
        path: root.display().to_string(),
        source,
    })?;
    let parent = parent.canonicalize().map_err(|source| BaselineError::Io {
        path: parent.display().to_string(),
        source,
    })?;
    if parent.starts_with(&root) {
        Ok(())
    } else {
        Err(BaselineError::OutsideRoot {
            path: parent.display().to_string(),
        })
    }
}

fn atomic_write(path: &Path, contents: &str) -> Result<(), BaselineError> {
    let parent = path.parent().ok_or_else(|| BaselineError::Io {
        path: path.display().to_string(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing parent directory"),
    })?;
    let original_metadata = fs::metadata(path).ok();
    let mut temp = tempfile::Builder::new()
        .prefix(".chokkin-baseline-")
        .tempfile_in(parent)
        .map_err(|source| BaselineError::Io {
            path: path.display().to_string(),
            source,
        })?;
    temp.write_all(contents.as_bytes())
        .map_err(|source| BaselineError::Io {
            path: path.display().to_string(),
            source,
        })?;
    temp.as_file()
        .sync_all()
        .map_err(|source| BaselineError::Io {
            path: path.display().to_string(),
            source,
        })?;
    if let Some(metadata) = original_metadata {
        temp.as_file()
            .set_permissions(metadata.permissions())
            .map_err(|source| BaselineError::Io {
                path: path.display().to_string(),
                source,
            })?;
    }
    temp.persist(path).map_err(|error| BaselineError::Io {
        path: path.display().to_string(),
        source: error.error,
    })?;
    Ok(())
}

fn build_summary(issues: &[Issue]) -> IssueSummary {
    let mut by_rule = std::collections::BTreeMap::new();
    for issue in issues {
        *by_rule.entry(issue.rule).or_insert(0) += 1;
    }
    IssueSummary {
        total: u32::try_from(issues.len()).unwrap_or(u32::MAX),
        by_rule,
    }
}

fn compute_exit_status(issues: &[Issue], previous: crate::ExitStatus) -> crate::ExitStatus {
    if previous == crate::ExitStatus::Success || issues.is_empty() {
        crate::ExitStatus::Success
    } else {
        crate::ExitStatus::IssuesFound
    }
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn generated_at() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("unix:{}", duration.as_secs()),
        Err(_) => "unix:0".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Confidence;
    use crate::rules::{Issue, IssueLocation, IssueSubject, RuleId, Severity};
    use tempfile::TempDir;

    fn issue(path: &str) -> Issue {
        Issue {
            rule: RuleId::Chk001,
            severity: Severity::Warning,
            confidence: Confidence::Certain,
            message: "unused".to_owned(),
            workspace_member: None,
            location: IssueLocation {
                file: Some(path.to_owned()),
                line: Some(10),
                manifest: None,
            },
            subject: IssueSubject::File {
                path: path.to_owned(),
            },
            explain: None,
        }
    }

    #[test]
    fn fingerprint_uses_normalized_path_without_line() {
        assert_eq!(
            issue_fingerprint(&issue("src\\legacy.py")),
            "CHK001:src/legacy.py"
        );
    }

    #[test]
    fn symbol_fingerprint_prefers_path_and_symbol() {
        let mut issue = issue("src\\acme\\api.py");
        issue.rule = RuleId::Chk006;
        issue.subject = IssueSubject::Symbol {
            module: "acme.api".to_owned(),
            name: "public_api".to_owned(),
        };
        assert_eq!(
            issue_fingerprint(&issue),
            "CHK006:src/acme/api.py:public_api"
        );
    }

    #[test]
    fn fingerprint_includes_workspace_member() {
        let mut issue = issue("src\\legacy.py");
        issue.workspace_member = Some("api".to_owned());
        assert_eq!(issue_fingerprint(&issue), "CHK001:api:src/legacy.py");
    }

    #[test]
    fn baseline_does_not_suppress_other_workspace_member() {
        let dir = TempDir::new().expect("tempdir");
        let baseline = dir.path().join("chokkin-baseline.json");
        let mut frozen = issue("src/shared.py");
        frozen.workspace_member = Some("api".to_owned());
        let frozen_report = IssueReport {
            issues: vec![frozen],
            suppressed: Vec::new(),
            summary: IssueSummary {
                total: 1,
                by_rule: std::iter::once((RuleId::Chk001, 1)).collect(),
            },
            exit_status: crate::ExitStatus::IssuesFound,
        };
        write_baseline(&frozen_report, dir.path(), &baseline).expect("write baseline");

        let mut current = issue("src/shared.py");
        current.workspace_member = Some("worker".to_owned());
        let mut current_report = IssueReport {
            issues: vec![current],
            suppressed: Vec::new(),
            summary: IssueSummary {
                total: 1,
                by_rule: std::iter::once((RuleId::Chk001, 1)).collect(),
            },
            exit_status: crate::ExitStatus::IssuesFound,
        };

        let result =
            apply_baseline(&mut current_report, dir.path(), &baseline).expect("apply baseline");
        assert_eq!(result.suppressed, 0);
        assert_eq!(current_report.issues.len(), 1);
        assert_eq!(current_report.exit_status, crate::ExitStatus::IssuesFound);
    }

    #[test]
    fn baseline_filters_matching_issues() {
        let dir = TempDir::new().expect("tempdir");
        let baseline = dir.path().join("chokkin-baseline.json");
        let mut report = IssueReport {
            issues: vec![issue("src/legacy.py")],
            suppressed: Vec::new(),
            summary: IssueSummary {
                total: 1,
                by_rule: std::iter::once((RuleId::Chk001, 1)).collect(),
            },
            exit_status: crate::ExitStatus::IssuesFound,
        };
        write_baseline(&report, dir.path(), &baseline).expect("write baseline");
        let result = apply_baseline(&mut report, dir.path(), &baseline).expect("apply baseline");
        assert_eq!(result.suppressed, 1);
        assert!(report.issues.is_empty());
        assert_eq!(report.exit_status, crate::ExitStatus::Success);
        assert_eq!(report.suppressed[0].reason, SuppressReason::Baseline);
    }

    #[test]
    fn missing_nested_baseline_is_not_an_error() {
        let dir = TempDir::new().expect("tempdir");
        let baseline = dir.path().join(".chokkin").join("baseline.json");
        let mut report = IssueReport {
            issues: vec![issue("src/legacy.py")],
            suppressed: Vec::new(),
            summary: IssueSummary::default(),
            exit_status: crate::ExitStatus::IssuesFound,
        };

        let result = apply_baseline(&mut report, dir.path(), &baseline).expect("apply baseline");

        assert_eq!(result.suppressed, 0);
        assert_eq!(report.issues.len(), 1);
    }

    #[test]
    fn write_baseline_creates_missing_parent_directory() {
        let dir = TempDir::new().expect("tempdir");
        let baseline = dir.path().join(".chokkin").join("baseline.json");
        let report = IssueReport {
            issues: vec![issue("src/legacy.py")],
            suppressed: Vec::new(),
            summary: IssueSummary {
                total: 1,
                by_rule: std::iter::once((RuleId::Chk001, 1)).collect(),
            },
            exit_status: crate::ExitStatus::IssuesFound,
        };

        let result = write_baseline(&report, dir.path(), &baseline).expect("write baseline");
        let written = read_baseline(&baseline).expect("read baseline");

        assert_eq!(result.written, 1);
        assert_eq!(written.issues[0].fingerprint, "CHK001:src/legacy.py");
        assert_eq!(written.issues[0].target, "src/legacy.py");
        assert!(baseline.exists());
    }

    #[test]
    fn baseline_path_must_stay_inside_root() {
        let root = TempDir::new().expect("root");
        let outside = TempDir::new().expect("outside");
        let mut report = IssueReport {
            issues: vec![issue("src/legacy.py")],
            suppressed: Vec::new(),
            summary: IssueSummary::default(),
            exit_status: crate::ExitStatus::IssuesFound,
        };
        let error = apply_baseline(
            &mut report,
            root.path(),
            &outside.path().join("baseline.json"),
        )
        .expect_err("outside root");
        assert!(matches!(error, BaselineError::OutsideRoot { .. }));
    }
}
