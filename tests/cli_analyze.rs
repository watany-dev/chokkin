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

#[test]
fn binary_json_import_issues_carry_distribution_and_module() {
    for (fixture, code, module, distribution) in [
        ("missing_yaml", "CHK003", "yaml", "pyyaml"),
        ("transitive_urllib3", "CHK004", "urllib3", "urllib3"),
    ] {
        let issues = json_issues(&fixture_path(&["deps", fixture]), &[]);
        let issue = issues
            .iter()
            .find(|issue| issue["code"] == code)
            .expect("import issue");
        assert_eq!(issue["distribution"], distribution, "{fixture}");
        assert_eq!(issue["symbol"], module, "{fixture}");
        assert_eq!(issue["path"], "src/acme/main.py", "{fixture}");
        assert_eq!(issue["line"], 2, "{fixture}");
    }
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
fn binary_version_guarded_stdlib_import_is_not_chk010() {
    // #358: `tomllib` is stdlib on 3.11+, which `>=3.10,<3.15` covers.
    let temp = tempfile::TempDir::new().expect("tempdir");
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"guarded\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10,<3.15\"\ndependencies = [\"tomli; python_version < '3.11'\"]\n\n[project.scripts]\nguarded = \"guarded:main\"\n",
        ),
        (
            "guarded.py",
            "import sys\n\nif sys.version_info >= (3, 11):\n    import tomllib\nelse:\n    import tomli as tomllib\n\n\ndef main() -> None:\n    tomllib.loads(\"\")\n",
        ),
    ] {
        fs::write(temp.path().join(file), text).expect("write fixture");
    }
    let keys = issue_keys(&json_issues(temp.path(), &[]));
    assert!(
        keys.iter().all(|(_, target)| !target.contains("tomllib")),
        "{keys:?}"
    );
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
    assert_ne!(before, Vec::<serde_json::Value>::new());
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

#[test]
fn binary_scoped_declarations_resolve_normalized_roots() {
    // The PEP 723 block and the member manifest declare these names for their
    // own files only; the root manifest and lockfile know neither (#361).
    let temp = tempfile::TempDir::new().expect("tempdir");
    let files = [
        (
            "pyproject.toml",
            "[project]\nname = \"scoped-root\"\nversion = \"0.1.0\"\n\n[project.scripts]\napi-cli = \"api.main:main\"\n\n[tool.uv.workspace]\nmembers = [\"services/*\"]\n",
        ),
        (
            "services/api/pyproject.toml",
            "[project]\nname = \"api\"\nversion = \"0.1.0\"\ndependencies = [\"Member_Dep\"]\n",
        ),
        ("services/api/src/api/__init__.py", ""),
        (
            "services/api/src/api/main.py",
            "import member_dep\n\n\ndef main() -> None:\n    member_dep.run()\n",
        ),
        (
            "scripts/tool.py",
            "# /// script\n# dependencies = [\"Foo_Bar\"]\n# ///\nimport foo_bar\nimport other_local_mod\n\nfoo_bar.run(other_local_mod)\n",
        ),
    ];
    for (file, text) in files {
        let path = temp.path().join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        fs::write(path, text).expect("write fixture");
    }

    let keys = issue_keys(&json_issues(temp.path(), &[]));
    let unresolved = |root: &str| {
        keys.iter()
            .any(|(code, target)| code == "CHK010" && target.ends_with(root))
    };
    assert!(unresolved("other_local_mod"), "{keys:?}");
    assert!(!unresolved("foo_bar"), "{keys:?}");
    assert!(!unresolved("member_dep"), "{keys:?}");
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

/// Written at test time: a checked-in root `tests/__init__.py` would sit under
/// the repository's own `tests/` and not be a project root package.
fn root_tests_package_project() -> tempfile::TempDir {
    write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/core.py", "def run() -> int:\n    return 1\n"),
        ("tests/__init__.py", ""),
        (
            "tests/helpers.py",
            "def make_client() -> int:\n    return 1\n",
        ),
        ("tests/_support/__init__.py", ""),
        ("tests/_support/client.py", "class Gateway:\n    pass\n"),
        ("tests/orphan.py", "def orphan() -> int:\n    return 1\n"),
        (
            "tests/test_a.py",
            "from acme.core import run\nfrom tests._support.client import Gateway\nfrom tests.helpers import make_client\n\n\ndef test_a() -> None:\n    assert Gateway\n    assert make_client() == run()\n",
        ),
    ])
}

#[test]
fn binary_root_tests_package_imports_resolve_first_party() {
    let project = root_tests_package_project();
    let keys = issue_keys(&json_issues(project.path(), &[]));
    assert!(keys.iter().all(|(code, _)| code != "CHK010"), "{keys:?}");
    // Helpers are reached through `tests.*` imports, and test functions are
    // not reported as unused exports.
    let flagged: Vec<_> = keys
        .iter()
        .filter(|(code, target)| {
            code == "CHK001" || (code == "CHK006" && target.starts_with("tests/"))
        })
        .map(|(_, target)| target.as_str())
        .collect();
    assert_eq!(flagged, ["tests/orphan.py"], "{keys:?}");
}

fn write_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("tempdir");
    for (file, text) in files {
        let path = temp.path().join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        fs::write(path, text).expect("write fixture");
    }
    temp
}

/// litellm's `tests/e2e`: a conftest without `__init__.py` puts its directory
/// on `sys.path`, so tests below import siblings by top-level name (#360).
fn pytest_prepend_project(pytest_options: &str) -> tempfile::TempDir {
    let pyproject = format!(
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[tool.chokkin]\nmode = \"app\"\n\n[tool.pytest.ini_options]\n{pytest_options}"
    );
    write_project(&[
        ("pyproject.toml", pyproject.as_str()),
        ("acme/__init__.py", ""),
        (
            "tests/e2e/conftest.py",
            "from lifecycle import ResourceManager\n",
        ),
        (
            "tests/e2e/lifecycle.py",
            "class ResourceManager:\n    pass\n",
        ),
        (
            "tests/e2e/models/__init__.py",
            "from models.user import User\n",
        ),
        ("tests/e2e/models/user.py", "class User:\n    pass\n"),
        (
            "tests/e2e/access/test_access.py",
            "from lifecycle import ResourceManager\nfrom models import User\n\n\ndef test_access() -> None:\n    assert ResourceManager\n    assert User\n",
        ),
    ])
}

#[test]
fn binary_pytest_prepend_mode_resolves_test_local_imports() {
    let project = pytest_prepend_project("");
    let keys = issue_keys(&json_issues(project.path(), &[]));
    assert!(
        keys.iter().all(|(code, target)| code != "CHK010"
            && !(code == "CHK001" && target.starts_with("tests/"))),
        "{keys:?}"
    );
}

#[test]
fn binary_pytest_importlib_mode_does_not_prepend_test_dirs() {
    let project = pytest_prepend_project("addopts = \"--import-mode=importlib\"\n");
    let keys = issue_keys(&json_issues(project.path(), &[]));
    assert!(
        keys.iter()
            .any(|(code, target)| code == "CHK010" && target.ends_with("lifecycle")),
        "{keys:?}"
    );
}

/// langchain ships a deliberately non-UTF-8 fixture; one such file must not
/// abort the run (#486).
#[test]
fn binary_undecodable_source_is_skipped_with_a_diagnostic() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("tests/test_latin.py", "import acme.latin\n"),
        ("acme/__init__.py", ""),
        ("acme/helper.py", ""),
        ("acme/orphan.py", ""),
    ]);
    fs::write(
        project.path().join("acme/latin.py"),
        b"# -*- coding: latin-1 -*-\nimport acme.helper\ns = 'caf\xe9'\n",
    )
    .expect("write latin-1 source");
    fs::write(
        project.path().join("acme/cyrillic.py"),
        b"# coding: iso-8859-5\nu = '\xd0\xd1'\n",
    )
    .expect("write iso-8859-5 source");

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::IssuesFound.code().into())
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("skipped `acme/cyrillic.py`"), "{stderr}");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let keys = issue_keys(parsed["issues"].as_array().expect("issues"));
    // The latin-1 module is decoded and its import followed; the skipped one
    // is not reported as unused.
    let unused_files: Vec<_> = keys
        .iter()
        .filter(|(code, _)| code == "CHK001")
        .map(|(_, target)| target.as_str())
        .collect();
    assert_eq!(unused_files, ["acme/orphan.py"], "{keys:?}");
    assert!(
        parsed["diagnostics"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("acme/cyrillic.py")),
        "{parsed}"
    );
}

#[test]
fn reachable_undecodable_source_lowers_unused_findings_to_likely() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("tests/test_cyrillic.py", "import acme.cyrillic\n"),
        ("acme/__init__.py", ""),
        ("acme/orphan.py", ""),
    ]);
    // It may import `requests` or `acme.orphan`; nobody can tell.
    fs::write(
        project.path().join("acme/cyrillic.py"),
        b"# coding: iso-8859-5\nimport requests\nu = '\xd0\xd1'\n",
    )
    .expect("write iso-8859-5 source");

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let confidences: Vec<_> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .filter(|issue| matches!(issue["code"].as_str(), Some("CHK001" | "CHK002")))
        .map(|issue| (issue["code"].clone(), issue["confidence"].clone()))
        .collect();
    assert_eq!(
        confidences,
        [
            ("CHK001".into(), "likely".into()),
            ("CHK002".into(), "likely".into())
        ],
        "{parsed}"
    );
}
