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
    assert!(stderr.contains("file-removal-denied: file removal requires `--allow-remove-files`"));
}

#[test]
fn binary_fix_dry_run_reports_planned_not_applied() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    copy_dir_recursive(&fixture_path(&["deps", "unused_boto3"]), temp.path()).expect("copy");
    let pyproject = temp.path().join("pyproject.toml");
    let before = fs::read_to_string(&pyproject).expect("read pyproject");

    let dry_run = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--fix")
        .arg("--dry-run")
        .arg(temp.path())
        .output()
        .expect("run chokkin --fix --dry-run");
    let stderr = String::from_utf8(dry_run.stderr).expect("utf8");
    assert!(stderr.contains("  planned CHK002 boto3"), "{stderr}");
    assert!(!stderr.contains("  applied CHK"), "{stderr}");
    assert_eq!(
        fs::read_to_string(&pyproject).expect("read pyproject"),
        before
    );

    let fix = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--fix")
        .arg(temp.path())
        .output()
        .expect("run chokkin --fix");
    let stderr = String::from_utf8(fix.stderr).expect("utf8");
    assert!(stderr.contains("  applied CHK002 boto3"), "{stderr}");
    assert!(!stderr.contains("  planned CHK"), "{stderr}");
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

#[test]
fn binary_scoped_exact_declaration_beats_affixed_root_name() {
    // `pyzzfoo` / `zzbar-py` at the root only loosely match `zzfoo` / `zzbar`;
    // the script block and member manifest declare those names exactly.
    let temp = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"scoped-root\"\nversion = \"0.1.0\"\ndependencies = [\"pyzzfoo\", \"zzbar-py\"]\n\n[project.scripts]\napi-cli = \"api.main:main\"\n\n[tool.uv.workspace]\nmembers = [\"services/*\"]\n",
        ),
        (
            "services/api/pyproject.toml",
            "[project]\nname = \"api\"\nversion = \"0.1.0\"\ndependencies = [\"zzbar\"]\n",
        ),
        ("services/api/src/api/__init__.py", ""),
        (
            "services/api/src/api/main.py",
            "import zzbar\n\n\ndef main() -> None:\n    zzbar.run()\n",
        ),
        (
            "scripts/tool.py",
            "# /// script\n# dependencies = [\"zzfoo\"]\n# ///\nimport zzfoo\n\nzzfoo.run()\n",
        ),
    ]);
    let keys = issue_keys(&json_issues(temp.path(), &[]));
    assert!(
        keys.iter()
            .all(|(_, target)| !target.starts_with("script:")),
        "{keys:?}"
    );
    for root_dep in ["pyzzfoo", "zzbar-py"] {
        assert!(
            keys.contains(&("CHK002".to_owned(), root_dep.to_owned())),
            "{keys:?}"
        );
    }
}

#[test]
fn binary_declared_candidate_wins_among_distributions_sharing_a_root() {
    // `pydantic-ai` and `pydantic-ai-slim` both provide `pydantic_ai`; each
    // site takes the one its own manifest declares, not the map's first (#732).
    let temp = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"shared-root\"\nversion = \"0.1.0\"\ndependencies = [\"pydantic-ai-slim>=1.0\"]\n\n[project.scripts]\napi-cli = \"api.main:main\"\nroot-cli = \"shared_root:main\"\n\n[tool.uv.workspace]\nmembers = [\"services/*\"]\n",
        ),
        (
            "shared_root/__init__.py",
            "import pydantic_ai\n\n\ndef main() -> None:\n    pydantic_ai.run()\n",
        ),
        (
            "services/api/pyproject.toml",
            "[project]\nname = \"api\"\nversion = \"0.1.0\"\ndependencies = [\"pydantic-ai>=1.0\"]\n",
        ),
        ("services/api/src/api/__init__.py", ""),
        (
            "services/api/src/api/main.py",
            "import pydantic_ai\n\n\ndef main() -> None:\n    pydantic_ai.run()\n",
        ),
    ]);
    let keys = issue_keys(&json_issues(temp.path(), &[]));
    assert!(
        keys.iter()
            .all(|(code, _)| !matches!(code.as_str(), "CHK002" | "CHK003")),
        "{keys:?}"
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

/// pandas-style `acme/tests/`, a root `test/` and pip-style `_vendor/` (#490).
#[test]
fn binary_in_package_tests_and_vendored_code_are_not_reported() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        (
            "acme/core.py",
            "from acme._vendor.six import used\n\n\ndef run() -> int:\n    return used()\n\n\ndef dead() -> int:\n    return 0\n",
        ),
        (
            "acme/util.py",
            "def only_from_root_test() -> int:\n    return 1\n",
        ),
        ("acme/_vendor/__init__.py", ""),
        (
            "acme/_vendor/six.py",
            "def used() -> int:\n    return 1\n\n\ndef dead_vendored() -> int:\n    return 0\n",
        ),
        ("acme/tests/__init__.py", ""),
        (
            "acme/tests/test_core.py",
            "from acme.core import run\n\n\ndef helper() -> int:\n    return 1\n\n\ndef test_run() -> None:\n    assert run() == 1\n",
        ),
        (
            "test/test_util.py",
            "from acme.util import only_from_root_test\n\n\ndef test_util() -> None:\n    assert only_from_root_test() == 1\n",
        ),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &[]));
    let flagged: Vec<_> = keys
        .iter()
        .filter(|(code, _)| matches!(code.as_str(), "CHK001" | "CHK006" | "CHK007"))
        .map(|(code, target)| format!("{code}:{target}"))
        .collect();
    assert_eq!(flagged, ["CHK006:acme/core.py:dead"], "{keys:?}");
}

/// A workspace member only dependency groups pull in (airflow's
/// `devel-common`) never ships, so `--production` drops it like the tests
/// that import it, while the default run still reaches it from `conftest.py`
/// (#613).
#[test]
fn binary_production_drops_members_only_dev_groups_reference() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"core\"]\n\n[dependency-groups]\ndev = [\"helpers\"]\n\n[tool.uv.sources]\ncore = { workspace = true }\nhelpers = { workspace = true }\n\n[tool.uv.workspace]\nmembers = [\"core\", \"e2e\", \"helpers\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("main.py", "import core\n\nprint(core)\n"),
        (
            "core/pyproject.toml",
            "[project]\nname = \"core\"\nversion = \"0.1.0\"\n\n[dependency-groups]\ndev = [\"helpers\"]\n",
        ),
        ("core/src/core/__init__.py", ""),
        // Nothing references this test member, so its runtime dependency on
        // `helpers` does not ship `helpers`.
        (
            "e2e/pyproject.toml",
            "[project]\nname = \"e2e\"\nversion = \"0.1.0\"\ndependencies = [\"helpers\"]\n",
        ),
        ("core/src/core/orphan.py", ""),
        (
            "helpers/pyproject.toml",
            "[project]\nname = \"helpers\"\nversion = \"0.1.0\"\ndependencies = [\"pytest\"]\n\n[project.scripts]\nbuild-docs = \"helpers.docs:main\"\n",
        ),
        ("helpers/src/helpers/__init__.py", ""),
        (
            "helpers/src/helpers/docs.py",
            "def main() -> None:\n    pass\n",
        ),
        ("helpers/src/helpers/fixtures.py", "import pytest\n"),
        (
            "helpers/src/helpers/plugin.py",
            "from helpers import fixtures\n",
        ),
        (
            "tests/conftest.py",
            "from helpers import plugin\n\nprint(plugin)\n",
        ),
    ]);
    for extra in [&[][..], &["--production"][..]] {
        let keys = issue_keys(&json_issues(project.path(), extra));
        assert!(
            keys.iter()
                .all(|(_, target)| !target.starts_with("helpers/")),
            "{extra:?}: {keys:?}"
        );
        assert!(
            keys.iter()
                .any(|key| key == &("CHK001".to_owned(), "core/src/core/orphan.py".to_owned())),
            "{extra:?}: {keys:?}"
        );
    }
    for (extra, expected) in [
        (&[][..], "Workspace: 3 members (3 inventoried)"),
        (
            &["--production"][..],
            "Workspace: 2 members (2 inventoried)",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .arg("--probe")
            .args(extra)
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stdout = String::from_utf8(output.stdout).expect("utf8");
        assert!(stdout.contains(expected), "{extra:?}: {stdout}");
    }
    // Unlike an undeclared monorepo, declared members cache their manifests.
    assert!(project.path().join("core/.chokkin").is_dir());
}

/// A member a runtime group pulls in through `include-group`, and a shipped
/// member nested inside a dev-only one, both stay under `--production` (#613).
#[test]
fn binary_production_keeps_members_shipped_through_groups_or_nesting() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"runtime-lib\"]\n\n[dependency-groups]\nbase = [\"lib\"]\nserver = [{ include-group = \"base\" }]\ndev = [\"tools\"]\n\n[tool.uv.workspace]\nmembers = [\"lib\", \"tools\", \"tools/runtime-lib\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        (
            "main.py",
            "import lib\nimport runtime_lib\n\nprint(lib, runtime_lib)\n",
        ),
        (
            "lib/pyproject.toml",
            "[project]\nname = \"lib\"\nversion = \"0.1.0\"\n",
        ),
        ("lib/src/lib/__init__.py", ""),
        ("lib/src/lib/orphan.py", ""),
        (
            "tools/pyproject.toml",
            "[project]\nname = \"tools\"\nversion = \"0.1.0\"\n",
        ),
        ("tools/src/tools/__init__.py", ""),
        ("tools/src/tools/orphan.py", ""),
        (
            "tools/runtime-lib/pyproject.toml",
            "[project]\nname = \"runtime-lib\"\nversion = \"0.1.0\"\n",
        ),
        ("tools/runtime-lib/src/runtime_lib/__init__.py", ""),
        ("tools/runtime-lib/src/runtime_lib/orphan.py", ""),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &["--production"]));
    for orphan in [
        "lib/src/lib/orphan.py",
        "tools/runtime-lib/src/runtime_lib/orphan.py",
    ] {
        assert!(
            keys.iter()
                .any(|key| key == &("CHK001".to_owned(), orphan.to_owned())),
            "{orphan}: {keys:?}"
        );
    }
    assert!(
        keys.iter()
            .all(|(_, target)| !target.starts_with("tools/src/")),
        "{keys:?}"
    );
}

/// A member only dev groups reference still ships when its classifiers, given
/// or dynamic, mark it as published, like airflow's `airflow-ctl`, but not
/// when they opt out of upload (#621).
#[test]
fn binary_production_keeps_published_members_only_dev_groups_reference() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[dependency-groups]\ndev = [\"acme-ctl\", \"dyn\", \"helpers\"]\n\n[tool.uv.workspace]\nmembers = [\"ctl\", \"dyn\", \"helpers\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("main.py", "print(1)\n"),
        (
            "ctl/pyproject.toml",
            "[project]\nname = \"acme-ctl\"\nversion = \"0.1.0\"\nclassifiers = [\"Framework :: Acme\"]\n",
        ),
        ("ctl/src/acme_ctl/__init__.py", ""),
        ("ctl/src/acme_ctl/orphan.py", ""),
        (
            "dyn/pyproject.toml",
            "[project]\nname = \"dyn\"\nversion = \"0.1.0\"\ndynamic = [\"classifiers\"]\n",
        ),
        ("dyn/src/dyn/__init__.py", ""),
        ("dyn/src/dyn/orphan.py", ""),
        (
            "helpers/pyproject.toml",
            "[project]\nname = \"helpers\"\nversion = \"0.1.0\"\nclassifiers = [\"Framework :: Acme\", \"Private :: Do Not Upload\"]\n",
        ),
        ("helpers/src/helpers/__init__.py", ""),
        ("helpers/src/helpers/orphan.py", ""),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &["--production"]));
    for orphan in ["ctl/src/acme_ctl/orphan.py", "dyn/src/dyn/orphan.py"] {
        assert!(
            keys.iter()
                .any(|key| key == &("CHK001".to_owned(), orphan.to_owned())),
            "{orphan}: {keys:?}"
        );
    }
    assert!(
        keys.iter()
            .all(|(_, target)| !target.starts_with("helpers/")),
        "{keys:?}"
    );
}

/// starlette imports `JSONResponse` from `starlette.responses`, not the
/// package root, so `--production` must keep the tests, and the modules only
/// they reach, as evidence of API use. A private module's name, and one only
/// an orphaned private module imports, stay reported (#588).
#[test]
fn binary_production_library_keeps_public_module_names_used_by_tests() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.chokkin]\nmode = \"library\"\n",
        ),
        (
            "acme/__init__.py",
            "from acme import _utils, responses\n\n__all__ = [\"_utils\", \"responses\"]\n",
        ),
        (
            "acme/responses.py",
            "class JSONResponse:\n    pass\n\n\nclass HTMLResponse:\n    pass\n\n\nclass Unused:\n    pass\n\n\nclass Orphaned:\n    pass\n",
        ),
        (
            "acme/templating.py",
            "from acme.responses import HTMLResponse\n\n\nclass Template(HTMLResponse):\n    pass\n",
        ),
        ("acme/_utils.py", "def collapse() -> None:\n    pass\n"),
        // Orphaned private module: its import is not an outside caller.
        ("acme/_legacy.py", "from acme.responses import Unused\n"),
        // Without `--production`, a public module nothing reaches is not a
        // caller either.
        ("acme/plugins.py", "from acme.responses import Orphaned\n"),
        (
            "tests/test_responses.py",
            "from acme._utils import collapse\nfrom acme.responses import JSONResponse\nfrom acme.templating import Template\n\n\ndef test_json() -> None:\n    collapse()\n    JSONResponse()\n    Template()\n",
        ),
    ]);
    for extra in [&[][..], &["--production"][..]] {
        let keys = issue_keys(&json_issues(project.path(), extra));
        let flagged: Vec<_> = keys
            .iter()
            .filter(|(code, _)| code == "CHK006")
            .map(|(_, target)| target.as_str())
            .collect();
        let expected: &[&str] = if extra.is_empty() {
            &["acme/responses.py:Orphaned", "acme/responses.py:Unused"]
        } else {
            &["acme/_utils.py:collapse", "acme/responses.py:Unused"]
        };
        assert_eq!(flagged, expected, "{extra:?}: {keys:?}");
    }
}

/// A library member's tests are kept as API evidence under an app root too.
#[test]
fn binary_production_library_member_keeps_names_used_by_its_tests() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n",
        ),
        ("main.py", "import acme_lib\n\nprint(acme_lib)\n"),
        (
            "libs/acme-lib/pyproject.toml",
            "[project]\nname = \"acme-lib\"\nversion = \"0.1.0\"\n",
        ),
        (
            "libs/acme-lib/acme_lib/__init__.py",
            "from acme_lib import responses\n\n__all__ = [\"responses\"]\n",
        ),
        (
            "libs/acme-lib/acme_lib/responses.py",
            "class JSONResponse:\n    pass\n",
        ),
        (
            "libs/acme-lib/tests/test_responses.py",
            "from acme_lib.responses import JSONResponse\n\n\ndef test_json() -> None:\n    JSONResponse()\n",
        ),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &["--production"]));
    assert!(keys.iter().all(|(code, _)| code != "CHK006"), "{keys:?}");
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

/// fastapi's `docs_src/` and urllib3's `dummyserver/` sit at the root,
/// outside the source globs, and airflow's prek scripts import a module
/// beside them (#589).
#[test]
fn binary_resolves_root_dirs_from_tests_and_script_siblings() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[project.scripts]\nacme = \"acme.run:main\"\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("src/acme/__init__.py", ""),
        // A runtime module runs under its package name, not as a script.
        (
            "src/acme/run.py",
            "from helpers import console\n\n\ndef main() -> None:\n    console()\n",
        ),
        ("src/acme/helpers.py", "def console() -> None:\n    pass\n"),
        ("docs_src/tutorial001/__init__.py", ""),
        ("docs_src/tutorial001/app.py", "APP = 1\n"),
        ("dummyserver/__init__.py", ""),
        ("dummyserver/server.py", "PORT = 1\n"),
        // `tests/__init__.py` makes the root the tests' basedir.
        ("tests/__init__.py", ""),
        (
            "tests/test_tutorial.py",
            "from docs_src.tutorial001.app import APP\nfrom dummyserver.server import PORT\n\n\ndef test_app() -> None:\n    assert APP and PORT\n",
        ),
        ("scripts/ci/prek/__init__.py", ""),
        (
            "scripts/ci/prek/check_thing.py",
            "from common_utils import console\n\nconsole()\n",
        ),
        (
            "scripts/ci/prek/common_utils.py",
            "def console() -> None:\n    pass\n",
        ),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &[]));
    let unresolved: Vec<&str> = keys
        .iter()
        .filter(|(code, _)| code == "CHK010")
        .map(|(_, target)| target.as_str())
        .collect();
    assert_eq!(unresolved, ["src/acme/run.py:helpers"], "{keys:?}");
}

/// airflow: `[tool.pytest]` (pytest 9) adds `example_*.py`, and the root
/// `testpaths` does not cover the member suites passed to pytest by path (#603).
#[test]
fn binary_pytest_native_python_files_cover_app_member_tests() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"root\"\nversion = \"0.1.0\"\n\n[tool.uv.workspace]\nmembers = [\"providers/*\"]\n\n[tool.pytest]\ntestpaths = [\"tests\"]\npython_files = [\"test_*.py\", \"example_*.py\"]\n",
        ),
        (
            "providers/foo/pyproject.toml",
            "[project]\nname = \"foo\"\nversion = \"0.1.0\"\n\n[project.scripts]\nfoo = \"foo.main:main\"\n",
        ),
        ("providers/foo/src/foo/__init__.py", ""),
        (
            "providers/foo/src/foo/main.py",
            "def main() -> None:\n    pass\n",
        ),
        ("providers/foo/src/foo/example_unused.py", "X = 1\n"),
        ("providers/foo/tests/system/example_dag.py", "DAG = 1\n"),
        ("providers/foo/tests/system/helpers.py", "X = 1\n"),
        // An existing `testpaths`, so pytest does not fall back to the rootdir.
        ("tests/test_root.py", ""),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &["--include", "CHK001"]));
    let reported = |path: &str| {
        keys.iter()
            .any(|(code, target)| code == "CHK001" && target == path)
    };
    assert!(
        !reported("providers/foo/tests/system/example_dag.py"),
        "{keys:?}"
    );
    assert!(
        reported("providers/foo/tests/system/helpers.py"),
        "{keys:?}"
    );
    // `python_files` reaches member tests only, not runtime modules.
    assert!(
        reported("providers/foo/src/foo/example_unused.py"),
        "{keys:?}"
    );
}

/// `llama_index`: hundreds of member pyprojects and no workspace declaration
/// (#488).
fn undeclared_monorepo() -> tempfile::TempDir {
    write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"llama-index\"\nversion = \"0.1.0\"\ndependencies = [\"llama-index-core\"]\n",
        ),
        (
            "llama-index-core/pyproject.toml",
            "[project]\nname = \"llama-index-core\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n",
        ),
        (
            "llama-index-core/llama_index/core/__init__.py",
            "from llama_index.core.base import BaseLLM\n",
        ),
        (
            "llama-index-core/llama_index/core/base.py",
            "import requests\n\n\nclass BaseLLM:\n    session = requests\n",
        ),
        (
            "llama-index-integrations/llms/llama-index-llms-openai/pyproject.toml",
            "[project]\nname = \"llama-index-llms-openai\"\nversion = \"0.1.0\"\ndependencies = [\"openai\", \"llama-index-core\"]\n\n[dependency-groups]\ndev = [\"pytest\"]\n",
        ),
        (
            "llama-index-integrations/llms/llama-index-llms-openai/llama_index/llms/openai/__init__.py",
            "from llama_index.llms.openai.base import OpenAI\n",
        ),
        (
            "llama-index-integrations/llms/llama-index-llms-openai/llama_index/llms/openai/base.py",
            "import openai\nfrom llama_index.core import BaseLLM\n\n\nclass OpenAI(BaseLLM):\n    client = openai\n",
        ),
        (
            "llama-index-integrations/llms/llama-index-llms-openai/tests/conftest.py",
            "import openai\nimport pytest\n",
        ),
        // App member: its own `[project.scripts]` is the only entry, and it
        // imports its own package by the distribution's underscore spelling.
        (
            "llama-dev/pyproject.toml",
            "[project]\nname = \"llama-dev\"\nversion = \"0.1.0\"\n\n[project.scripts]\nllama-dev = \"llama_dev.cli:cli\"\n",
        ),
        // Its lockfile lists the member itself, so `llama_dev` matches a
        // locked name before anything says it is local.
        (
            "llama-dev/uv.lock",
            "version = 1\n\n[[package]]\nname = \"llama-dev\"\nversion = \"0.1.0\"\nsource = { editable = \".\" }\n",
        ),
        ("llama-dev/llama_dev/__init__.py", ""),
        (
            "llama-dev/llama_dev/cli.py",
            "from llama_dev import utils\nfrom . import release\n\n\ndef cli():\n    utils.run()\n    release.run()\n",
        ),
        ("llama-dev/llama_dev/utils.py", "def run():\n    pass\n"),
        ("llama-dev/llama_dev/release.py", "def run():\n    pass\n"),
        ("llama-dev/llama_dev/orphan.py", ""),
        // Tool-only pyproject: not a member.
        ("docs/pyproject.toml", "[tool.ruff]\nline-length = 88\n"),
    ])
}

fn certain_chk001(issues: &[serde_json::Value]) -> Vec<String> {
    issues
        .iter()
        .filter(|issue| issue["code"] == "CHK001" && issue["confidence"] == "certain")
        .map(|issue| issue["target"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn binary_undeclared_monorepo_members_are_auto_detected() {
    let project = undeclared_monorepo();
    let issues = json_issues(project.path(), &[]);
    assert_eq!(
        certain_chk001(&issues),
        ["llama-dev/llama_dev/orphan.py"],
        "{issues:?}"
    );
    let keys = issue_keys(&issues);
    assert!(
        keys.iter().all(|(code, target)| !(matches!(
            code.as_str(),
            "CHK003" | "CHK004" | "CHK010"
        ) && (target == "openai"
            || target == "requests"
            || target.contains("llama_dev")))),
        "{keys:?}"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--probe")
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stdout.contains("Workspace: 3 members"), "{stdout}");
    assert!(
        stderr.contains("treating 3 nested pyproject.toml as workspace members"),
        "{stderr}"
    );
    assert!(project.path().join(".chokkin").is_dir());
    assert!(!project.path().join("llama-index-core/.chokkin").exists());
}

/// A declared member without an app entry ships its own wheel, so its
/// orphans are library-scored like a detected member's, even when only its
/// tests reach them and `--production` drops the tests. Files its wheel does
/// not ship keep app scoring (#587).
#[test]
fn binary_declared_workspace_library_member_is_scored_as_library() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"root\"\nversion = \"0.1.0\"\n\n[tool.uv.workspace]\nmembers = [\"providers/*\", \"core\"]\n",
        ),
        (
            "providers/google/pyproject.toml",
            "[build-system]\nrequires = [\"flit_core\"]\nbuild-backend = \"flit_core.buildapi\"\n\n[project]\nname = \"provider-google\"\nversion = \"0.1.0\"\n\n[project.entry-points.apache_airflow_provider]\nprovider_info = \"airflow.providers.google.get_provider_info:get_provider_info\"\n\n[tool.flit.module]\nname = \"airflow.providers.google\"\n",
        ),
        // Outside the member's wheel, so no outside caller imports it.
        ("providers/google/scripts/release.py", "VERSION = \"1\"\n"),
        // Namespace package: no `src/airflow/__init__.py`.
        (
            "providers/google/src/airflow/providers/google/__init__.py",
            "",
        ),
        (
            "providers/google/src/airflow/providers/google/hooks.py",
            "def hook():\n    pass\n",
        ),
        (
            "providers/google/tests/test_hooks.py",
            "from airflow.providers.google.hooks import hook\n\n\ndef test_hook():\n    hook()\n",
        ),
        // App member: its console script keeps app scoring.
        (
            "core/pyproject.toml",
            "[project]\nname = \"core\"\nversion = \"0.1.0\"\n\n[project.scripts]\ncore = \"core.main:main\"\n",
        ),
        ("core/src/core/__init__.py", ""),
        ("core/src/core/main.py", "def main():\n    pass\n"),
        ("core/src/core/orphan.py", ""),
    ]);
    let hooks = "providers/google/src/airflow/providers/google/hooks.py";
    for extra in [&[][..], &["--production"][..]] {
        let issues = json_issues(project.path(), extra);
        assert_eq!(
            certain_chk001(&issues),
            [
                "core/src/core/orphan.py",
                "providers/google/scripts/release.py"
            ],
            "{extra:?}: {issues:?}"
        );
        assert!(
            issues.iter().all(|issue| !(issue["code"] == "CHK001"
                && issue["target"] == hooks
                && issue["severity"] == "error")),
            "{extra:?}: {issues:?}"
        );
    }
}

/// Monorepos keep Sphinx docs per member: each member's `docs/conf.py` is a
/// docs entry, so it and the shared modules it imports stay reachable even
/// with no root `docs/conf.py`, and `--production` drops it like root docs
/// (#612).
#[test]
fn binary_member_sphinx_conf_is_a_docs_entry() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"root\"\nversion = \"0.1.0\"\n\n[tool.uv.workspace]\nmembers = [\"core\", \"providers/*\", \"devel-common\"]\n",
        ),
        (
            "core/pyproject.toml",
            "[project]\nname = \"core\"\nversion = \"0.1.0\"\n\n[project.scripts]\ncore = \"core.main:main\"\n",
        ),
        ("core/src/core/__init__.py", ""),
        ("core/src/core/main.py", "def main():\n    pass\n"),
        ("core/src/core/orphan.py", ""),
        (
            "providers/google/pyproject.toml",
            "[build-system]\nrequires = [\"flit_core\"]\nbuild-backend = \"flit_core.buildapi\"\n\n[project]\nname = \"provider-google\"\nversion = \"0.1.0\"\ndependencies = [\"sphinx\"]\n\n[tool.flit.module]\nname = \"google_provider\"\n",
        ),
        ("providers/google/src/google_provider/__init__.py", ""),
        (
            "providers/google/docs/conf.py",
            "from docs.provider_conf import *\n\nextensions = [\"sphinx_exts.redirects\"]\n",
        ),
        // The wheel ships `tests_common`; `docs` and `sphinx_exts` sit beside it.
        (
            "devel-common/pyproject.toml",
            "[build-system]\nrequires = [\"flit_core\"]\nbuild-backend = \"flit_core.buildapi\"\n\n[project]\nname = \"devel-common\"\nversion = \"0.1.0\"\n\n[tool.flit.module]\nname = \"tests_common\"\n",
        ),
        ("devel-common/src/tests_common/__init__.py", ""),
        ("devel-common/src/docs/__init__.py", ""),
        (
            "devel-common/src/docs/provider_conf.py",
            "project = \"provider\"\n",
        ),
        ("devel-common/src/sphinx_exts/__init__.py", ""),
        (
            "devel-common/src/sphinx_exts/redirects.py",
            "def setup(app):\n    pass\n",
        ),
    ]);
    let issues = json_issues(project.path(), &[]);
    assert_eq!(
        certain_chk001(&issues),
        ["core/src/core/orphan.py"],
        "{issues:?}"
    );
    let issues = json_issues(project.path(), &["--production"]);
    assert!(
        issues
            .iter()
            .all(|issue| issue["target"] != "providers/google/docs/conf.py"),
        "{issues:?}"
    );
}

#[test]
fn binary_no_auto_workspace_keeps_single_project_analysis() {
    let project = undeclared_monorepo();
    let issues = json_issues(project.path(), &["--no-auto-workspace"]);
    assert!(!certain_chk001(&issues).is_empty(), "{issues:?}");
}

/// A latin-1 `setup.py` is read through its PEP 263 declaration, and one
/// without a declaration is skipped with a warning; neither aborts the run
/// (#552).
#[test]
fn binary_non_utf8_setup_py_does_not_abort_the_run() {
    let pyproject = "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndynamic = [\"dependencies\"]\n\n[tool.chokkin]\nmode = \"app\"\n";
    let declared = write_project(&[
        ("pyproject.toml", pyproject),
        ("src/acme/__init__.py", "import requests\n"),
        ("tests/test_acme.py", "import acme\n"),
    ]);
    fs::write(
        declared.path().join("setup.py"),
        b"# -*- coding: latin-1 -*-\nfrom setuptools import setup\nsetup(name='acme', author='Ren\xe9', install_requires=['requests'])\n",
    )
    .expect("write latin-1 setup.py");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(declared.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::Success.code().into()),
        "{stderr}"
    );
    assert!(!stderr.contains("manifest:"), "{stderr}");

    let undeclared = write_project(&[
        ("pyproject.toml", pyproject),
        ("src/acme/__init__.py", "import requests\n"),
        ("tests/test_acme.py", "import acme\n"),
    ]);
    fs::write(
        undeclared.path().join("setup.py"),
        b"from setuptools import setup\nsetup(name='acme', author='Ren\xe9', install_requires=['requests'])\n",
    )
    .expect("write undeclared latin-1 setup.py");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(undeclared.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::Success.code().into()),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "manifest: skipped `setup.py`: not UTF-8 and no supported PEP 263 coding declaration"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("runtime dependencies in `setup.py` could not be read"),
        "{stderr}"
    );
    // CHK003 for `requests` is downgraded, so no certain issue is reported.
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let keys = issue_keys(parsed["issues"].as_array().expect("issues"));
    assert!(keys.iter().all(|(code, _)| code != "CHK003"), "{keys:?}");
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

#[test]
fn binary_malformed_dynamic_import_names_do_not_abort_analysis() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[project.scripts]\nacme = \"acme:main\"\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        (
            "src/acme/__init__.py",
            "import importlib\ndef main(): pass\nimportlib.import_module(\".sub\", __package__)\nimportlib.import_module(\"\", \"acme\")\nimportlib.import_module(\"a..b\")\nimportlib.import_module(\"pkg.\")\n",
        ),
        ("src/acme/sub.py", ""),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::Success.code().into()),
        "{stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    // `.sub` resolves against `__package__`, so `sub.py` is reachable.
    assert_eq!(parsed["issues"], serde_json::json!([]), "{parsed}");
}

/// An app root over an auto-detected library member: the member ships its own
/// wheel, so its unused public API is judged as a library's (#515).
#[test]
fn binary_library_member_symbols_use_library_mode() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n",
        ),
        ("main.py", "from acme_lib import helper\n\nhelper()\n"),
        (
            "libs/acme-lib/pyproject.toml",
            "[project]\nname = \"acme-lib\"\nversion = \"0.1.0\"\n",
        ),
        (
            "libs/acme-lib/acme_lib/__init__.py",
            "from ._impl import helper\n",
        ),
        (
            "libs/acme-lib/acme_lib/_impl/__init__.py",
            "from .core import helper, Extra\n",
        ),
        (
            "libs/acme-lib/acme_lib/_impl/core.py",
            "def helper():\n    pass\n\n\ndef lib_dead():\n    pass\n\n\nclass Extra:\n    pass\n",
        ),
    ]);
    let issues = json_issues(project.path(), &[]);
    let severity = |code: &str, target: &str| {
        issues
            .iter()
            .find(|issue| issue["code"] == code && issue["target"] == target)
            .map(|issue| issue["severity"].as_str().unwrap_or_default().to_owned())
    };
    for (code, target) in [
        ("CHK006", "libs/acme-lib/acme_lib/_impl/core.py:lib_dead"),
        ("CHK007", "libs/acme-lib/acme_lib/_impl/__init__.py:Extra"),
    ] {
        assert_eq!(
            severity(code, target).as_deref(),
            Some("info"),
            "{issues:?}"
        );
    }
}

/// mcp-python-sdk / fastmcp: a workspace root and a member with its own CLI
/// resolve to app, yet each wheel ships its package's `__all__` as public
/// API, so those re-exports are not CHK007. A name outside `__all__`, and an
/// `__all__` its own member's wheel does not ship, stay reported (#678).
#[test]
fn binary_wheel_all_reexports_are_not_chk007_in_app_mode() {
    let hatch = "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n";
    let root = format!(
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[project.scripts]\nacme = \"acme.cli:main\"\n\n{hatch}\n[tool.hatch.build.targets.wheel]\npackages = [\"src/acme\", \"src/demo-app\"]\n\n[tool.uv.workspace]\nmembers = [\"src/acme-cli\", \"src/demo-app\"]\n"
    );
    let member = format!(
        "[project]\nname = \"acme-cli\"\nversion = \"0.1.0\"\n\n[project.scripts]\nacme-cli = \"acme_cli.main:main\"\n\n{hatch}\n[tool.hatch.build.targets.wheel]\npackages = [\"acme_cli\"]\n"
    );
    let project = write_project(&[
        ("pyproject.toml", root.as_str()),
        (
            "src/acme/__init__.py",
            "from ._core import Thing, Other\n\n__all__ = [\"Thing\"]\n",
        ),
        (
            "src/acme/_core.py",
            "class Thing:\n    pass\n\n\nclass Other:\n    pass\n",
        ),
        (
            "src/acme/cli.py",
            "import acme\n\n\ndef main():\n    print(acme)\n",
        ),
        ("src/acme-cli/pyproject.toml", member.as_str()),
        (
            "src/acme-cli/acme_cli/__init__.py",
            "from .runner import Runner\n\n__all__ = [\"Runner\"]\n",
        ),
        (
            "src/acme-cli/acme_cli/runner.py",
            "class Runner:\n    pass\n",
        ),
        (
            "src/acme-cli/acme_cli/main.py",
            "import acme_cli\n\n\ndef main():\n    print(acme_cli)\n",
        ),
        // No wheel targets of its own: nothing ships this `__all__`, even
        // though the root's targets cover the member directory.
        (
            "src/demo-app/pyproject.toml",
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[project.scripts]\ndemo = \"demo.app:main\"\n",
        ),
        (
            "src/demo-app/demo/__init__.py",
            "from .impl import Tool\n\n__all__ = [\"Tool\"]\n",
        ),
        ("src/demo-app/demo/impl.py", "class Tool:\n    pass\n"),
        (
            "src/demo-app/demo/app.py",
            "import demo\n\n\ndef main():\n    print(demo)\n",
        ),
    ]);
    let chk007: Vec<_> = issue_keys(&json_issues(project.path(), &[]))
        .into_iter()
        .filter(|(code, _)| code == "CHK007")
        .map(|(_, target)| target)
        .collect();
    assert_eq!(
        chk007,
        [
            "src/acme/__init__.py:Other",
            "src/demo-app/demo/__init__.py:Tool"
        ],
        "{chk007:?}"
    );
}

/// Two members force app mode; a shipped module's `__all__` is its public API
/// even when only the module itself uses a name (#727, `fastmcp_slim`).
#[test]
fn shipped_all_names_are_not_chk006_in_app_mode() {
    let hatch = "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n";
    let root = format!(
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[project.scripts]\nacme = \"acme.cli:main\"\n\n{hatch}\n[tool.hatch.build.targets.wheel]\npackages = [\"src/acme\"]\n\n[tool.uv.workspace]\nmembers = [\"plugins/one\", \"plugins/two\"]\n"
    );
    let member = |name: &str| {
        format!(
            "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n{hatch}\n[tool.hatch.build.targets.wheel]\npackages = [\"{name}\"]\n"
        )
    };
    let (one, two) = (member("one"), member("two"));
    let project = write_project(&[
        ("pyproject.toml", root.as_str()),
        (
            "src/acme/__init__.py",
            "from . import limits\n\n__all__ = [\"limits\"]\n",
        ),
        (
            "src/acme/limits.py",
            "__all__ = [\"MAX\", \"check\"]\n\nMAX = 3\nSPARE = 4\n\n\ndef check(value):\n    return value < MAX\n",
        ),
        (
            "src/acme/cli.py",
            "from acme.limits import check\n\n\ndef main():\n    print(check(1))\n",
        ),
        ("plugins/one/pyproject.toml", one.as_str()),
        ("plugins/one/one/__init__.py", ""),
        ("plugins/two/pyproject.toml", two.as_str()),
        ("plugins/two/two/__init__.py", ""),
    ]);
    let chk006: Vec<_> = issue_keys(&json_issues(project.path(), &[]))
        .into_iter()
        .filter(|(code, _)| code == "CHK006")
        .map(|(_, target)| target)
        .collect();
    assert_eq!(chk006, ["src/acme/limits.py:SPARE"], "{chk006:?}");
}

/// A member without wheel targets ships what hatchling auto-detects, its
/// `src/<name>` package (#733, `llama-index-instrumentation`).
#[test]
fn layout_shipped_member_all_reexports_are_not_chk007() {
    let hatch = "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n";
    let root = "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.uv.workspace]\nmembers = [\"instr\", \"other\"]\n";
    let instr = format!(
        "[project]\nname = \"instr\"\nversion = \"0.1.0\"\n\n[project.scripts]\ninstr = \"instr:main\"\n\n{hatch}"
    );
    let project = write_project(&[
        ("pyproject.toml", root),
        ("instr/pyproject.toml", instr.as_str()),
        (
            "instr/src/instr/__init__.py",
            "from .base import Event\n\n\ndef main():\n    print(Event)\n",
        ),
        (
            "instr/src/instr/base/__init__.py",
            "from .event import Event\nfrom .handler import Handler\n\n__all__ = [\"Event\", \"Handler\"]\n",
        ),
        ("instr/src/instr/base/event.py", "class Event:\n    pass\n"),
        (
            "instr/src/instr/base/handler.py",
            "class Handler:\n    pass\n",
        ),
        (
            "other/pyproject.toml",
            "[project]\nname = \"other\"\nversion = \"0.1.0\"\n",
        ),
        ("other/other/__init__.py", ""),
    ]);
    let chk007: Vec<_> = issue_keys(&json_issues(project.path(), &[]))
        .into_iter()
        .filter(|(code, _)| code == "CHK007")
        .collect();
    assert!(chk007.is_empty(), "{chk007:?}");
}

/// `llama_index`'s azurepostgresql member: `psycopg[pool]` brings in
/// `psycopg-pool` through the lock's `optional-dependencies` (#516). An
/// undeclared monorepo locks each member; a uv workspace locks at the root.
fn extras_member_project(requirement: &str, uv_workspace: bool) -> tempfile::TempDir {
    let (root, lock) = if uv_workspace {
        (
            "[tool.uv.workspace]\nmembers = [\"vector-stores\"]\n",
            "uv.lock",
        )
    } else {
        ("", "vector-stores/uv.lock")
    };
    write_project(&[
        (
            "pyproject.toml",
            &format!("[project]\nname = \"llama-index\"\nversion = \"0.1.0\"\n\n{root}"),
        ),
        (
            "vector-stores/pyproject.toml",
            &format!(
                "[project]\nname = \"vector-stores\"\nversion = \"0.1.0\"\ndependencies = [\"{requirement}\"]\n\n[project.scripts]\nvs = \"vector_stores.cli:main\"\n"
            ),
        ),
        (
            lock,
            "version = 1\n\n\
             [[package]]\nname = \"vector-stores\"\nversion = \"0.1.0\"\nsource = { editable = \".\" }\n\
             dependencies = [{ name = \"psycopg\", extra = [\"pool\"] }]\n\n\
             [[package]]\nname = \"psycopg\"\nversion = \"3.2.0\"\n\n\
             [package.optional-dependencies]\npool = [{ name = \"psycopg-pool\" }]\n\n\
             [[package]]\nname = \"psycopg-pool\"\nversion = \"3.2.0\"\n",
        ),
        ("vector-stores/vector_stores/__init__.py", ""),
        (
            "vector-stores/vector_stores/cli.py",
            "import psycopg\nimport psycopg_pool\n\n\ndef main():\n    return psycopg, psycopg_pool\n",
        ),
    ])
}

#[test]
fn binary_extra_distributions_count_as_declared() {
    for uv_workspace in [false, true] {
        let undeclared = |requirement: &str| {
            let project = extras_member_project(requirement, uv_workspace);
            json_issues(project.path(), &[])
                .into_iter()
                .filter(|issue| {
                    matches!(issue["code"].as_str(), Some("CHK003" | "CHK004"))
                        && issue["distribution"] == "psycopg-pool"
                })
                .count()
        };

        assert_eq!(undeclared("psycopg[binary,Pool]~=3.0"), 0, "{uv_workspace}");
        assert_eq!(undeclared("psycopg[binary]~=3.0"), 1, "{uv_workspace}");
        assert_eq!(undeclared("psycopg~=3.0"), 1, "{uv_workspace}");
    }
}

#[test]
fn binary_notebooks_are_entry_roots() {
    let notebook = r#"{"cells": [{"cell_type": "code", "source": ["from acme import plotting\n", "plotting.draw()\n"]}], "metadata": {}, "nbformat": 4, "nbformat_minor": 5}"#;
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.chokkin]\nmode = \"app\"\n\n[project.scripts]\nacme = \"acme.cli:main\"\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/cli.py", "def main():\n    pass\n"),
        ("acme/plotting.py", "def draw():\n    pass\n"),
        ("acme/orphan.py", ""),
        ("acme/demo.ipynb", notebook),
    ]);
    let issues = json_issues(project.path(), &[]);
    let orphans: Vec<_> = issue_keys(&issues)
        .into_iter()
        .filter(|(code, _)| code == "CHK001")
        .map(|(_, target)| target)
        .collect();
    assert_eq!(orphans, ["acme/orphan.py"], "{issues:?}");
}

/// black's `tests/data/cases/*.py`: tests read these as files, so an orphan
/// there is expected; a fixture module imported by conftest stays reachable
/// (#593).
#[test]
fn binary_unreachable_test_data_is_reported_only_in_strict_mode() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.chokkin]\nmode = \"app\"\n\n[project.scripts]\nacme = \"acme.cli:main\"\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/cli.py", "def main():\n    pass\n"),
        ("tests/__init__.py", ""),
        (
            "tests/conftest.py",
            "from tests.fixtures.helpers import make\n",
        ),
        ("tests/fixtures/__init__.py", ""),
        ("tests/fixtures/helpers.py", "def make():\n    pass\n"),
        ("tests/data/case.py", "x = 1\n"),
        ("tests/orphan.py", ""),
    ]);
    let orphans = |extra: &[&str]| -> Vec<String> {
        issue_keys(&json_issues(project.path(), extra))
            .into_iter()
            .filter(|(code, _)| code == "CHK001")
            .map(|(_, target)| target)
            .collect()
    };
    assert_eq!(orphans(&[]), ["tests/orphan.py"]);
    assert_eq!(
        orphans(&["--strict"]),
        ["tests/data/case.py", "tests/orphan.py"]
    );
}

/// fastmcp declares its extras through the uv-dynamic-versioning metadata hook
/// (#590): they must count as declared, not leave imports unresolved or
/// declared only in the dev group.
#[test]
fn uv_dynamic_versioning_hook_extras_count_as_declared() {
    let temp = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\ndynamic = [\"version\", \"optional-dependencies\"]\ndependencies = []\n\n[dependency-groups]\ndev = [\"pyperclip>=1.9\"]\n\n[tool.hatch.metadata.hooks.uv-dynamic-versioning.optional-dependencies]\nclient = [\"acme[clip]=={{ version }}\", \"py-key-value-aio>=0.4\"]\nclip = [\"pyperclip>=1.9\"]\n",
        ),
        (
            "acme/__init__.py",
            "import pyperclip\nfrom key_value.aio.stores.memory import MemoryStore\n",
        ),
    ]);

    let keys = issue_keys(&json_issues(temp.path(), &["--no-cache"]));
    assert!(
        keys.iter()
            .all(|(code, _)| !matches!(code.as_str(), "CHK003" | "CHK005" | "CHK010")),
        "{keys:?}"
    );
}

#[test]
fn requirements_outside_fixed_names_declare_imports_for_chk010_only() {
    // werkzeug `requirements/tests.in`, urllib3 `emscripten-requirements.txt`
    // and mlflow `requirements/*-requirements.txt` (#679).
    let temp = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n",
        ),
        (
            "requirements/tests.in",
            "ephemeral-port-reserve\n-r base.txt\n",
        ),
        ("requirements/base.txt", "zzwidget-py\n"),
        ("requirements/ml-requirements.txt", "catboost\n"),
        ("emscripten-requirements.txt", "pytest-pyodide\n"),
        (
            "acme/__init__.py",
            "import requests\nimport catboost\n\nrequests.get(catboost)\n",
        ),
        (
            "tests/test_app.py",
            "import ephemeral_port_reserve\nimport pytest_pyodide\nimport zzwidget\nimport zzmissing\n",
        ),
    ]);

    for args in [&["--no-cache"][..], &["--no-cache", "--strict"][..]] {
        let keys = issue_keys(&json_issues(temp.path(), args));
        let unresolved: Vec<&str> = keys
            .iter()
            .filter(|(code, _)| code == "CHK010")
            .map(|(_, target)| target.as_str())
            .collect();
        assert_eq!(unresolved.len(), 1, "{keys:?}");
        assert!(unresolved[0].ends_with("zzmissing"), "{keys:?}");
        // The files give no context, so they neither declare nor leave unused.
        assert!(
            keys.iter().all(|(code, _)| !matches!(
                code.as_str(),
                "CHK001" | "CHK002" | "CHK003" | "CHK005"
            )),
            "{keys:?}"
        );
    }
}

#[test]
fn binary_examples_feed_reachability_only_and_nested_projects_are_skipped() {
    // #694: examples run outside the package, and a nested `[project]` that
    // is not a workspace member has its own dependencies.
    let temp = tempfile::TempDir::new().expect("tempdir");
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.uv.workspace]\nmembers = [\"packages/core\"]\n",
        ),
        (
            "examples/demo.py",
            "import yaml\nimport nosuchmodule_root\n",
        ),
        (
            "examples/app/pyproject.toml",
            "[project]\nname = \"app\"\nversion = \"0.1.0\"\n",
        ),
        ("examples/app/server.py", "import nosuchmodule_app\n"),
        (
            "packages/core/pyproject.toml",
            "[project]\nname = \"core\"\nversion = \"0.1.0\"\n",
        ),
        (
            "packages/core/examples/run.py",
            "import nosuchmodule_member\n",
        ),
    ] {
        let path = temp.path().join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write fixture");
    }
    let issues = json_issues(temp.path(), &[]);
    let summary = issues
        .iter()
        .map(|issue| {
            (
                issue["target"].as_str().unwrap_or_default(),
                issue["severity"].as_str().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            ("examples/demo.py:nosuchmodule_root", "info"),
            ("packages/core/examples/run.py:nosuchmodule_member", "info"),
        ],
        "{issues:#?}"
    );
}

#[test]
fn binary_checks_inline_scripts_inside_skipped_nested_projects() {
    // #714: a PEP 723 script declares its own dependencies, so skipping the
    // nested project around it must not skip the script.
    let temp = tempfile::TempDir::new().expect("tempdir");
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.uv.workspace]\nmembers = [\"packages/core\"]\n",
        ),
        (
            "packages/core/pyproject.toml",
            "[project]\nname = \"core\"\nversion = \"0.1.0\"\n",
        ),
        (
            "clients/python/pyproject.toml",
            "[project]\nname = \"client\"\nversion = \"0.1.0\"\n",
        ),
        (
            "clients/python/client/api.py",
            "import nosuchmodule_client\n",
        ),
        (
            "clients/python/check.py",
            "# /// script\n# dependencies = [\"rich\"]\n# ///\nimport rich\nimport yaml\n",
        ),
    ] {
        let path = temp.path().join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write fixture");
    }
    let issues = json_issues(temp.path(), &[]);
    let summary = issues
        .iter()
        .map(|issue| {
            (
                issue["code"].as_str().unwrap_or_default(),
                issue["target"].as_str().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [("CHK003", "script:clients/python/check.py:pyyaml")],
        "{issues:#?}"
    );
}
