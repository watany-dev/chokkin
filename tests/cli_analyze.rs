//! Integration tests for the full analysis CLI (Phase 1).

use std::path::PathBuf;
use std::process::Command;

use yokei::ExitStatus;

fn fixture_path(parts: &[&str]) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for part in parts {
        path.push(part);
    }
    path
}

#[test]
fn binary_analyze_unused_dependency_exits_one() {
    let root = fixture_path(&["deps", "unused_boto3"]);
    let output = Command::new(env!("CARGO_BIN_EXE_yokei"))
        .arg(&root)
        .output()
        .expect("run yokei");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::IssuesFound.code().into())
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("Unused dependencies"));
    assert!(stdout.contains("Summary:"));
}

#[test]
fn binary_no_exit_code_returns_zero_with_issues() {
    let root = fixture_path(&["deps", "unused_boto3"]);
    let output = Command::new(env!("CARGO_BIN_EXE_yokei"))
        .arg("--no-exit-code")
        .arg(&root)
        .output()
        .expect("run yokei");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::Success.code().into())
    );
}

#[test]
fn binary_json_reporter_outputs_schema_fields() {
    let root = fixture_path(&["deps", "unused_boto3"]);
    let output = Command::new(env!("CARGO_BIN_EXE_yokei"))
        .arg("--reporter")
        .arg("json")
        .arg(&root)
        .output()
        .expect("run yokei");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("\"issues\""));
    assert!(stdout.contains("\"YOK002\""));
    assert!(stdout.contains("\"summary\""));
}

#[test]
fn binary_probe_mode_still_available() {
    let root = fixture_path(&["probe", "empty"]);
    let output = Command::new(env!("CARGO_BIN_EXE_yokei"))
        .arg("--probe")
        .arg(&root)
        .output()
        .expect("run yokei");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("(probe)"));
}

#[test]
fn binary_explain_prints_to_stderr() {
    let root = fixture_path(&["deps", "unused_boto3"]);
    let output = Command::new(env!("CARGO_BIN_EXE_yokei"))
        .arg("--explain")
        .arg("YOK002:boto3")
        .arg(&root)
        .output()
        .expect("run yokei");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("boto3"));
}

#[test]
fn binary_dry_run_without_fix_errors() {
    let output = Command::new(env!("CARGO_BIN_EXE_yokei"))
        .arg("--dry-run")
        .output()
        .expect("run yokei");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::UsageError.code().into())
    );
}
