//! Baseline read, filter, and atomic write operations.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::VERSION;
use crate::rules::{
    Issue, IssueReport, IssueSubject, IssueSummary, SuppressReason, SuppressedIssue,
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
    let issues = report
        .issues
        .iter()
        .map(|issue| BaselineEntry {
            fingerprint: issue_fingerprint(issue),
            code: issue.rule.as_code().to_owned(),
            target: issue_target(issue),
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
    let parent = path.parent().unwrap_or(root.as_path());
    let parent = parent.canonicalize().map_err(|source| BaselineError::Io {
        path: path.display().to_string(),
        source,
    })?;
    if !parent.starts_with(&root) {
        return Err(BaselineError::OutsideRoot {
            path: path.display().to_string(),
        });
    }
    Ok(path)
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

fn issue_fingerprint(issue: &Issue) -> String {
    format!("{}:{}", issue.rule.as_code(), issue_target(issue))
}

fn issue_target(issue: &Issue) -> String {
    match &issue.subject {
        IssueSubject::File { path } => normalize_path(path),
        IssueSubject::Distribution { name } | IssueSubject::Binary { name } => name.clone(),
        IssueSubject::Symbol { module, name } => issue
            .location
            .file
            .as_deref()
            .map_or_else(|| format!("{module}:{name}"), |path| {
                format!("{}:{name}", normalize_path(path))
            }),
        IssueSubject::Import { module, file, .. } => {
            format!("{}:{module}", normalize_path(file))
        },
    }
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
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
    use crate::rules::{IssueLocation, RuleId, Severity};
    use tempfile::TempDir;

    fn issue(path: &str) -> Issue {
        Issue {
            rule: RuleId::Chk001,
            severity: Severity::Warning,
            confidence: Confidence::Certain,
            message: "unused".to_owned(),
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
    fn baseline_path_must_stay_inside_root() {
        let root = TempDir::new().expect("root");
        let outside = TempDir::new().expect("outside");
        let mut report = IssueReport {
            issues: vec![issue("src/legacy.py")],
            suppressed: Vec::new(),
            summary: IssueSummary::default(),
            exit_status: crate::ExitStatus::IssuesFound,
        };
        let error =
            apply_baseline(&mut report, root.path(), &outside.path().join("baseline.json"))
                .expect_err("outside root");
        assert!(matches!(error, BaselineError::OutsideRoot { .. }));
    }
}
