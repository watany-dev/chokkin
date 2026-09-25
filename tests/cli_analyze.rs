//! Integration tests for the full analysis CLI (Phase 1).

#![allow(clippy::expect_used)]

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
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(parsed["schema_version"], "1");
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

fn json_issues(root: &std::path::Path, extra: &[&str]) -> Vec<serde_json::Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--reporter", "json"])
        .args(extra)
        .arg(root)
        .output()
        .expect("run chokkin");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    parsed["issues"].as_array().cloned().unwrap_or_default()
}

fn issue_keys(issues: &[serde_json::Value]) -> Vec<(String, String)> {
    issues
        .iter()
        .map(|issue| {
            (
                issue["code"].as_str().unwrap_or_default().to_owned(),
                issue["target"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

const PEP723_TOOL: &str = r#"# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "rich",
#   "httpx",
# ]
# ///
import tomllib

import yaml
from rich import print as rich_print

from acme.helpers import shout


def _main() -> None:
    rich_print(shout(str(tomllib.loads(""))))
    yaml.safe_load("key: value")


if __name__ == "__main__":
    _main()
"#;

/// Written at test time: a checked-in copy would be analyzed by the
/// repository's own chokkin baseline run, where the script is an entry.
fn pep723_project() -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let files = [
        (
            "pyproject.toml",
            "[project]\nname = \"pep723-script\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = [\"requests\"]\n\n[project.scripts]\nacme-cli = \"acme.main:main\"\n",
        ),
        ("src/acme/__init__.py", ""),
        (
            "src/acme/main.py",
            "import requests\n\n\ndef main() -> None:\n    requests.get(\"https://example.com\")\n",
        ),
        (
            "src/acme/helpers.py",
            "def shout(text: str) -> str:\n    return text.upper()\n",
        ),
        ("scripts/tool.py", PEP723_TOOL),
    ];
    for (file, text) in files {
        let path = temp.path().join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        fs::write(path, text).expect("write fixture");
    }
    temp
}

#[test]
fn binary_pep723_script_reports_script_scoped_dependencies() {
    let project = pep723_project();
    for extra in [&[][..], &["--strict"][..]] {
        let issues = json_issues(project.path(), extra);
        let keys = issue_keys(&issues);
        assert!(
            keys.contains(&(
                "CHK003".to_owned(),
                "script:scripts/tool.py:pyyaml".to_owned()
            )),
            "{keys:?}"
        );
        assert!(
            keys.contains(&(
                "CHK002".to_owned(),
                "script:scripts/tool.py:httpx".to_owned()
            )),
            "{keys:?}"
        );
        // rich is declared by the script; tomllib is stdlib under the script's
        // `>=3.11` even though the project targets 3.10; yaml is not a project
        // CHK003; helpers.py is reachable from the script entry.
        assert!(
            keys.iter().all(|(code, target)| {
                (code != "CHK003" || target.starts_with("script:"))
                    && code != "CHK001"
                    && !target.contains("tomllib")
                    && !target.contains("rich")
            }),
            "{keys:?}"
        );
        let script_issue = issues
            .iter()
            .find(|issue| issue["target"] == "script:scripts/tool.py:pyyaml")
            .expect("script issue");
        assert_eq!(script_issue["path"], "scripts/tool.py");
        assert_eq!(script_issue["distribution"], "pyyaml");
        assert_eq!(script_issue["file"], "scripts/tool.py");
        assert_eq!(script_issue["line"], 10);
    }
}

#[test]
fn binary_pep723_script_findings_in_sarif() {
    let project = pep723_project();
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--reporter", "sarif"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("\"ruleId\": \"CHK003\""));
    assert!(stdout.contains("\"uri\": \"scripts/tool.py\""));
}

#[test]
fn binary_pep723_script_findings_follow_baseline_and_config_ignore() {
    let temp = pep723_project();
    let baseline = temp.path().join("chokkin-baseline.json");
    let baseline_arg = baseline.to_string_lossy().into_owned();

    let before = json_issues(
        temp.path(),
        &["--baseline", &baseline_arg, "--update-baseline"],
    );
    assert!(!before.is_empty());
    let baseline_contents = fs::read_to_string(&baseline).expect("read baseline");
    assert!(baseline_contents.contains("script:scripts/tool.py:pyyaml"));
    let after = json_issues(temp.path(), &["--baseline", &baseline_arg]);
    assert!(issue_keys(&after).is_empty(), "{:?}", issue_keys(&after));

    let pyproject = temp.path().join("pyproject.toml");
    let mut manifest = fs::read_to_string(&pyproject).expect("read pyproject");
    manifest.push_str(
        "\n[tool.chokkin.ignore]\nCHK002 = [\"script:scripts/tool.py:httpx\"]\nCHK003 = [\"script:scripts/tool.py:pyyaml\"]\n",
    );
    fs::write(&pyproject, manifest).expect("write pyproject");
    let ignored = issue_keys(&json_issues(temp.path(), &[]));
    assert!(
        ignored
            .iter()
            .all(|(_, target)| !target.starts_with("script:")),
        "{ignored:?}"
    );
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
