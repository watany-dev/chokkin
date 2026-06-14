//! Integration tests for CLI probe mode.

use std::path::PathBuf;
use std::process::Command;

use chokkin::{
    ExitStatus, RuntimeOverrides, probe_project, write_probe_report, write_probe_warnings,
};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn probe_src_layout_fixture() {
    let root = fixture_path("sources/src_layout");
    let report = probe_project(&root, None, &RuntimeOverrides::default()).expect("probe");
    assert_eq!(report.manifest.metadata.name.as_deref(), Some("acme"));
    assert!(report.sources.python_files().count() > 0);
}

#[test]
fn probe_empty_project_succeeds() {
    let root = fixture_path("probe/empty");
    let report = probe_project(&root, None, &RuntimeOverrides::default()).expect("probe");
    assert_eq!(report.manifest.dependencies.len(), 0);
    assert_eq!(report.sources.python_files().count(), 0);
}

#[test]
fn probe_broken_pyproject_errors() {
    let root = fixture_path("manifest/broken_pyproject");
    let err = probe_project(&root, None, &RuntimeOverrides::default()).expect_err("error");
    assert!(err.is_usage_error());
}

#[test]
fn probe_report_contains_expected_sections() {
    let root = fixture_path("sources/src_layout");
    let report = probe_project(&root, None, &RuntimeOverrides::default()).expect("probe");
    let mut output = Vec::new();
    write_probe_report(&report, &mut output).expect("write");
    let text = String::from_utf8(output).expect("utf8");
    assert!(text.contains("Manifest"));
    assert!(text.contains("Sources"));
    assert!(text.contains("probe complete"));
}

#[test]
fn probe_report_contains_resolved_workspace_count() {
    let root = fixture_path("config/uv_workspace_hint");
    let report = probe_project(&root, None, &RuntimeOverrides::default()).expect("probe");
    assert_eq!(report.workspace_members.len(), 2);
    let mut output = Vec::new();
    write_probe_report(&report, &mut output).expect("write");
    let text = String::from_utf8(output).expect("utf8");
    assert!(text.contains("Workspace: 2 members"));
}

#[test]
fn probe_warnings_written_to_stderr_format() {
    let root = fixture_path("sources/missing_entry");
    let report = probe_project(&root, None, &RuntimeOverrides::default()).expect("probe");
    assert!(!report.warnings.is_empty());
    let mut output = Vec::new();
    write_probe_warnings(&report.warnings, &mut output).expect("write");
    let text = String::from_utf8(output).expect("utf8");
    assert!(text.contains("sources:"));
}

#[test]
fn binary_help_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--help")
        .output()
        .expect("run chokkin");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("Usage: chokkin"));
}

#[test]
fn binary_probe_fixture_exits_zero() {
    let root = fixture_path("probe/empty");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg(&root)
        .output()
        .expect("run chokkin");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::Success.code().into())
    );
}

#[test]
fn binary_broken_pyproject_exits_two() {
    let root = fixture_path("manifest/broken_pyproject");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg(&root)
        .output()
        .expect("run chokkin");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::UsageError.code().into())
    );
}
