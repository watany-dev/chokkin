//! Integration tests for the full analysis CLI (Phase 1).

use std::path::PathBuf;
use std::process::Command;
use std::{fs, io};

use chokkin::ExitStatus;

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
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg(&root)
        .output()
        .expect("run chokkin");
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
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--no-exit-code")
        .arg(&root)
        .output()
        .expect("run chokkin");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::Success.code().into())
    );
}

#[test]
fn binary_json_reporter_outputs_schema_fields() {
    let root = fixture_path(&["deps", "unused_boto3"]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--reporter")
        .arg("json")
        .arg(&root)
        .output()
        .expect("run chokkin");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("\"issues\""));
    assert!(stdout.contains("\"CHK002\""));
    assert!(stdout.contains("\"summary\""));
}

#[test]
fn binary_github_reporter_outputs_annotations() {
    let root = fixture_path(&["deps", "unused_boto3"]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--reporter")
        .arg("github")
        .arg(&root)
        .output()
        .expect("run chokkin");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("::error"));
    assert!(stdout.contains("file=pyproject.toml"));
    assert!(stdout.contains("title=CHK002 boto3"));
}

#[test]
fn binary_sarif_reporter_outputs_minimal_schema() {
    let root = fixture_path(&["deps", "unused_boto3"]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--reporter")
        .arg("sarif")
        .arg(&root)
        .output()
        .expect("run chokkin");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("\"version\": \"2.1.0\""));
    assert!(stdout.contains("\"ruleId\": \"CHK002\""));
    assert!(stdout.contains("\"uri\": \"pyproject.toml\""));
    assert!(stdout.contains("\"runs\""));
}

#[test]
fn binary_probe_mode_still_available() {
    let root = fixture_path(&["probe", "empty"]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--probe")
        .arg(&root)
        .output()
        .expect("run chokkin");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("(probe)"));
}

#[test]
fn binary_explain_prints_to_stderr() {
    let root = fixture_path(&["deps", "unused_boto3"]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--explain")
        .arg("CHK002:boto3")
        .arg(&root)
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("boto3"));
}

#[test]
fn binary_dry_run_without_fix_errors() {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--dry-run")
        .output()
        .expect("run chokkin");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::UsageError.code().into())
    );
}

#[test]
fn binary_fix_reports_skipped_detail() {
    let root = fixture_path(&["reachability", "chain_import"]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--fix")
        .arg(&root)
        .output()
        .expect("run chokkin");

    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("Fixes:"));
    assert!(stderr.contains("skipped CHK001"));
    assert!(stderr.contains("file removal requires `--allow-remove-files`"));
}

#[test]
fn binary_baseline_update_then_suppresses_existing_issue() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    copy_dir_recursive(&fixture_path(&["deps", "unused_boto3"]), temp.path()).expect("copy");
    let baseline = temp.path().join("chokkin-baseline.json");

    let update = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--baseline")
        .arg(&baseline)
        .arg("--update-baseline")
        .arg(temp.path())
        .output()
        .expect("run chokkin");
    assert_eq!(
        update.status.code(),
        Some(ExitStatus::IssuesFound.code().into())
    );
    let baseline_contents = fs::read_to_string(&baseline).expect("read baseline");
    assert!(baseline_contents.contains("CHK002:boto3"));

    let filtered = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--baseline")
        .arg(&baseline)
        .arg(temp.path())
        .output()
        .expect("run chokkin");
    assert_eq!(
        filtered.status.code(),
        Some(ExitStatus::Success.code().into())
    );
    let stdout = String::from_utf8(filtered.stdout).expect("utf8");
    assert!(stdout.contains("Summary: 0 issues"));
}

fn copy_dir_recursive(source: &std::path::Path, target: &std::path::Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target_path = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target_path)?;
        } else {
            fs::copy(entry.path(), target_path)?;
        }
    }
    Ok(())
}
