//! Integration tests for optional fix (pipeline step 13).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

use std::path::{Path, PathBuf};

use chokkin::{AnalyzeOptions, ExitStatus, RuleId, RuntimeOverrides, analyze_project};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deps")
        .join(name)
}

#[test]
fn fix_removes_certain_unused_dependency_from_pyproject() {
    let source = fixture("unused_boto3");
    let temp = tempfile::tempdir().expect("tempdir");
    copy_dir_recursive(&source, temp.path()).expect("copy fixture");

    let pyproject = temp.path().join("pyproject.toml");
    let before = std::fs::read_to_string(&pyproject).expect("read pyproject");
    assert!(before.contains("boto3"));

    let report = analyze_project(
        temp.path(),
        None,
        &RuntimeOverrides::default(),
        AnalyzeOptions {
            fix_enabled: true,
            ..AnalyzeOptions::default()
        },
    )
    .expect("analyze with fix");
    assert_eq!(report.issues.exit_status, ExitStatus::IssuesFound);

    let fix_report = report.fix.expect("fix report");
    assert_eq!(fix_report.applied.len(), 1);
    assert_eq!(fix_report.applied[0].rule, RuleId::Chk002);

    let after = std::fs::read_to_string(&pyproject).expect("read pyproject after fix");
    assert!(!after.contains("\"boto3\""));
    assert!(after.contains("requests"));
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
