//! Integration tests for the full analysis CLI (Phase 1).

#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
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

/// `scripts/wheel-smoke.py` runs each release wheel on the same fixture and
/// compares against the same file, so the shipped binaries match this build
/// (#834).
#[test]
fn binary_wheel_smoke_fixture_matches_expected_report() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let project = temp.path().join("wheel_smoke");
    copy_dir_recursive(&fixture_path(&["wheel_smoke"]), &project).expect("copy fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json", "."])
        .current_dir(&project)
        .output()
        .expect("run chokkin");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::IssuesFound.code().into())
    );
    let mut report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    let version = report
        .as_object_mut()
        .and_then(|fields| fields.remove("version"));
    assert_eq!(version, Some(env!("CARGO_PKG_VERSION").into()));

    let expected_path = fixture_path(&["wheel_smoke.json"]);
    let expected: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&expected_path).expect("read expected"))
            .expect("parse expected");
    assert_eq!(
        report,
        expected,
        "update {} to this output (without \"version\"):\n{:#}",
        expected_path.display(),
        report
    );

    let schema_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/schema/chokkin-report.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(schema_path).expect("read schema"))
            .expect("parse schema");
    let mut versioned = expected;
    versioned["version"] = env!("CARGO_PKG_VERSION").into();
    jsonschema::Validator::new(&schema)
        .expect("compile schema")
        .validate(&versioned)
        .expect("expected report matches the published schema");
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

/// A `pyproject.toml` symlinked outside the root: `--fix` refuses it, and
/// `--dry-run` must not plan what the run would refuse (#980).
#[cfg(unix)]
#[test]
fn binary_fix_dry_run_skips_target_outside_the_root() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let (root, outside) = symlinked_pyproject(temp.path());
    let before = fs::read_to_string(&outside).expect("read");

    for args in [&["--fix", "--dry-run"][..], &["--fix"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache"])
            .args(args)
            .arg(&root)
            .output()
            .expect("run chokkin --fix");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        assert!(
            stderr.contains(
                "skipped CHK002 requests — unsupported-target: fix target `pyproject.toml` resolves outside the project root"
            ),
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("planned CHK"), "{args:?}: {stderr}");
    }
    assert_eq!(fs::read_to_string(&outside).expect("read"), before);
}

/// `--init` appends to `pyproject.toml`, so it gets the same containment check
/// as `--fix` (#980).
#[cfg(unix)]
#[test]
fn binary_init_refuses_pyproject_outside_the_root() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let (root, outside) = symlinked_pyproject(temp.path());
    let before = fs::read_to_string(&outside).expect("read");

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--init")
        .arg(&root)
        .output()
        .expect("run chokkin --init");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::UsageError.code().into()),
        "{stderr}"
    );
    assert!(
        stderr.contains("init target `pyproject.toml` resolves outside the project root"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(&outside).expect("read"), before);
}

#[cfg(unix)]
fn symlinked_pyproject(base: &Path) -> (PathBuf, PathBuf) {
    let root = base.join("proj");
    let outside = base.join("outside").join("target_pyproject.toml");
    fs::create_dir_all(root.join("demo")).expect("mkdir");
    fs::create_dir_all(outside.parent().expect("parent")).expect("mkdir");
    fs::write(
        &outside,
        "[project]\nname = \"demo\"\nversion = \"0\"\ndependencies = [\"requests\"]\n",
    )
    .expect("write outside pyproject");
    fs::write(root.join("demo/__init__.py"), "").expect("write package");
    std::os::unix::fs::symlink(
        "../outside/target_pyproject.toml",
        root.join("pyproject.toml"),
    )
    .expect("symlink");
    (root, outside)
}

/// Links the walk does not follow are named with why, one warning each,
/// while a link inside the root is read at its target and stays silent
/// (#986). Built at test time: a committed `/etc/passwd` link would follow
/// the repository into every checkout and package.
#[cfg(unix)]
#[test]
fn binary_skipped_symlinks_are_reported_with_their_reason() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path().join("proj");
    let other = temp.path().join("other_project/pkg");
    fs::create_dir_all(root.join("demo")).expect("mkdir");
    fs::create_dir_all(&other).expect("mkdir");
    fs::write(other.join("mod.py"), "").expect("write");
    fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"demo\"\nversion = \"0\"\ndependencies = []\n",
    )
    .expect("write pyproject");
    fs::write(root.join("demo/__init__.py"), "").expect("write package");
    fs::write(root.join("demo/real.py"), "").expect("write module");
    symlink("b", root.join("a")).expect("symlink");
    symlink("a", root.join("b")).expect("symlink");
    symlink("../other_project/pkg", root.join("outside")).expect("symlink");
    symlink("/etc/passwd", root.join("secret.py")).expect("symlink");
    symlink("missing.py", root.join("gone.py")).expect("symlink");
    symlink("real.py", root.join("demo/alias.py")).expect("symlink");
    symlink("demo", root.join("demo_alias")).expect("symlink");

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(&root)
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    let skipped: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("did not follow symlink"))
        .collect();
    for expected in [
        "sources: did not follow symlink `a`: it forms a loop",
        "sources: did not follow symlink `b`: it forms a loop",
        "sources: did not follow symlink `outside`: it points outside the project root",
        "sources: did not follow symlink `secret.py`: it points outside the project root",
        "sources: did not follow symlink `gone.py`: it points to nothing",
    ] {
        assert!(
            skipped.iter().any(|line| line.contains(expected)),
            "{expected}: {stderr}"
        );
    }
    assert_eq!(skipped.len(), 5, "{stderr}");
}

/// A link that is neither dangling nor a loop is named with the OS error,
/// and one in an excluded directory is pruned before it is judged.
#[cfg(unix)]
#[test]
fn binary_symlink_errors_name_the_os_reason() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path().join("proj");
    let other = temp.path().join("other");
    fs::create_dir_all(root.join("demo")).expect("mkdir");
    fs::create_dir_all(&other).expect("mkdir");
    fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"demo\"\nversion = \"0\"\ndependencies = []\n",
    )
    .expect("write pyproject");
    fs::write(root.join("demo/__init__.py"), "").expect("write package");
    symlink("demo/__init__.py/x", root.join("odd.py")).expect("symlink");
    symlink("../other", root.join("build")).expect("symlink");

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(&root)
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    let skipped: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("did not follow symlink"))
        .collect();
    assert_eq!(skipped.len(), 1, "{stderr}");
    assert!(
        skipped[0].contains("did not follow symlink `odd.py`: Not a directory"),
        "{stderr}"
    );
}

/// A symlinked virtualenv answers to a directory pattern (`venv/`) in
/// `.gitignore`, as the directory it points to would.
#[cfg(unix)]
#[test]
fn binary_gitignored_directory_symlink_is_silent() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path().join("proj");
    let outside = temp.path().join("env");
    fs::create_dir_all(root.join(".git")).expect("mkdir");
    fs::create_dir_all(&outside).expect("mkdir");
    fs::write(root.join(".git/HEAD"), "").expect("write");
    fs::write(root.join(".gitignore"), "venv/\n").expect("write");
    fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"demo\"\nversion = \"0\"\ndependencies = []\n",
    )
    .expect("write pyproject");
    fs::write(root.join("app.py"), "").expect("write module");
    symlink(&outside, root.join("venv")).expect("symlink");

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(&root)
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(!stderr.contains("did not follow symlink"), "{stderr}");
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

/// Issue #712: a src-layout root inventories only `src/**`, so the member
/// files whose imports use another member are parsed for that check alone.
/// They go into the root's parse cache bundle, and an edit to one is seen on
/// the next warm run.
#[test]
fn binary_member_imports_outside_root_inventory_are_cached() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    copy_dir_recursive(
        &fixture_path(&["deps", "uv_workspace_member_import_src_layout"]),
        temp.path(),
    )
    .expect("copy");
    for _ in 0..2 {
        assert_eq!(issue_keys(&json_issues(temp.path(), &[])), []);
    }
    let bundles: Vec<String> = fs::read_dir(temp.path().join(".chokkin/cache/parse"))
        .expect("parse cache")
        .map(|entry| fs::read_to_string(entry.expect("entry").path()).expect("bundle"))
        .collect();
    assert_eq!(bundles.len(), 1);
    assert!(bundles[0].contains("packages/core/src/acme/core/__init__.py"));

    fs::write(
        temp.path().join("packages/core/src/acme/core/__init__.py"),
        "y = 1\n",
    )
    .expect("edit member");
    assert_eq!(
        issue_keys(&json_issues(temp.path(), &[])),
        [("CHK002".to_owned(), "acme-util".to_owned())]
    );
}

/// The member files parsed for that check report what the parser could not
/// read like root files do, since a skipped one hides its imports.
#[test]
fn binary_member_files_outside_root_inventory_report_parse_warnings() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    copy_dir_recursive(
        &fixture_path(&["deps", "uv_workspace_member_import_src_layout"]),
        temp.path(),
    )
    .expect("copy");
    fs::write(
        temp.path().join("packages/core/src/acme/core/__init__.py"),
        "from acme_util import y\ndef broken(:\n",
    )
    .expect("edit member");

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(temp.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains(
            "parse: skipped `packages/core/src/acme/core/__init__.py`: syntax error at line 2"
        ),
        "{stderr}"
    );
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

/// A plugin reference (`INSTALLED_APPS`, Sphinx `extensions`) carries the
/// line of its string and says which setting named it, while the fingerprint
/// keeps `file:module` (#830).
#[test]
fn binary_json_plugin_import_issues_carry_their_string_line() {
    for (fixture, label, lines) in [
        ("django_manage", "INSTALLED_APPS", vec![4]),
        ("sphinx_docs", "extensions", vec![4, 6, 8]),
    ] {
        let issues = json_issues(&fixture_path(&["plugins", fixture]), &["--no-cache"]);
        let unresolved = issues
            .iter()
            .filter(|issue| issue["code"] == "CHK010")
            .collect::<Vec<_>>();
        let found = unresolved
            .iter()
            .map(|issue| issue["line"].as_u64().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(found, lines, "{fixture}");
        for issue in unresolved {
            let message = issue["message"].as_str().unwrap_or_default();
            assert!(
                message.contains(&format!("`{label}` entry `")),
                "{fixture}: {message}"
            );
        }
    }

    let root = fixture_path(&["plugins", "sphinx_docs"]);
    let issues = json_issues(&root, &["--no-cache"]);
    assert!(
        issues
            .iter()
            .any(|issue| issue["fingerprint"] == "CHK010:docs/conf.py:myst_parser"),
        "{issues:?}"
    );
    for selector in [
        "CHK010:docs/conf.py:myst_parser",
        "CHK010:docs/conf.py:4:myst_parser",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--explain", selector])
            .arg(&root)
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        assert!(
            stderr.contains("`myst_parser` is not stdlib"),
            "{selector}: {stderr}"
        );
    }
}

/// With a line, an `extensions` entry takes an inline ignore on its string
/// like any import (#830).
#[test]
fn binary_inline_ignore_covers_a_plugin_reference_line() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let files = [
        (
            "pyproject.toml",
            "[project]\nname = \"docs\"\nversion = \"0.1.0\"\ndependencies = [\"sphinx\"]\n\n\
             [tool.chokkin.plugins]\nsphinx = true\n",
        ),
        (
            "docs/conf.py",
            "extensions = [\n    \"myst_parser\",  # chokkin: ignore[CHK010]\n    \"sphinx_design\",\n]\n",
        ),
    ];
    for (file, text) in files {
        let path = temp.path().join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        fs::write(path, text).expect("write fixture");
    }

    let issues = json_issues(temp.path(), &["--no-cache"]);
    let unresolved = issues
        .iter()
        .filter(|issue| issue["code"] == "CHK010")
        .map(|issue| issue["symbol"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(unresolved, ["sphinx_design"]);
}

/// An undeclared distribution named only in `INSTALLED_APPS` is CHK003 at the
/// string's line, and its message and explain name the setting, not an
/// `import` (#830).
#[test]
fn binary_plugin_missing_dependency_names_its_setting() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let files = [
        (
            "pyproject.toml",
            "[project]\nname = \"mysite\"\nversion = \"0.1.0\"\ndependencies = [\"django>=4.0\"]\n",
        ),
        (
            "manage.py",
            "import os\n\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"mysite.settings\")\n",
        ),
        ("mysite/__init__.py", ""),
        (
            "mysite/settings.py",
            "INSTALLED_APPS = [\n    \"django.contrib.admin\",\n    \"requests\",\n    \"requests.adapters\",\n]\n",
        ),
    ];
    for (file, text) in files {
        let path = temp.path().join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        fs::write(path, text).expect("write fixture");
    }

    let issues = json_issues(temp.path(), &["--no-cache"]);
    // Entries of one distribution on separate lines stay one issue, as when
    // they had no line: the fingerprint set must not grow.
    let missing: Vec<_> = issues
        .iter()
        .filter(|issue| issue["code"] == "CHK003")
        .collect();
    assert_eq!(missing.len(), 1, "{missing:?}");
    let missing = missing[0];
    assert_eq!(missing["line"], 3);
    let message = missing["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("referenced requests from `INSTALLED_APPS` in mysite/settings.py:3"),
        "{message}"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--explain", "CHK003:requests"])
        .arg(temp.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("`INSTALLED_APPS` entry at mysite/settings.py:3"),
        "{stderr}"
    );
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

/// #984: a PEP 723 block declares the environment `uv run` builds, so it
/// lists runtime peers (marimo's dataframe backends, transformers' `torch`)
/// that the script never imports. CHK002 says so as likely, not certain.
#[test]
fn pep723_unused_dependency_is_likely() {
    let project = pep723_project();
    for (extra, severity) in [(&[][..], "warning"), (&["--strict"][..], "error")] {
        let issues = json_issues(project.path(), extra);
        let httpx = issues
            .iter()
            .find(|issue| issue["target"] == "script:scripts/tool.py:httpx")
            .expect("httpx unused");
        assert_eq!(httpx["confidence"], "likely", "{extra:?}");
        assert_eq!(httpx["severity"], severity, "{extra:?}");
    }
}

/// #984: transformers imports `accelerate` itself for `device_map="auto"`.
#[test]
fn accelerate_is_used_when_transformers_is() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"transformers\", \"accelerate\"]\n\n[project.scripts]\nacme = \"acme:main\"\n",
        ),
        (
            "acme.py",
            "import transformers\n\n\ndef main() -> None:\n    transformers.pipeline(\"text-generation\", device_map=\"auto\")\n",
        ),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &["--strict"]));
    assert!(!keys.iter().any(|(code, _)| code == "CHK002"), "{keys:?}");
}

#[test]
fn pep723_runtime_peers_are_used_without_hiding_unrelated_dependencies() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"transformers\"\nversion = \"0.1.0\"\n",
        ),
        ("src/transformers/__init__.py", ""),
        (
            "scripts/train.py",
            "# /// script\n# dependencies = [\"transformers\", \"accelerate\", \"torch\", \"sentencepiece\", \"protobuf\", \"evaluate\", \"rouge-score\", \"boto3\"]\n# ///\nimport transformers\nimport evaluate\ntransformers.pipeline(\"summarization\")\nevaluate.load(\"rouge\")\n",
        ),
    ]);
    for extra in [&[][..], &["--strict"][..]] {
        let unused: Vec<_> = issue_keys(&json_issues(project.path(), extra))
            .into_iter()
            .filter(|(code, _)| code == "CHK002")
            .collect();
        assert_eq!(
            unused,
            [(
                "CHK002".to_owned(),
                "script:scripts/train.py:boto3".to_owned()
            )]
        );
    }
}

#[test]
fn pep723_command_argument_list_uses_the_program_and_its_runtime_peers() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n",
        ),
        ("src/acme/__init__.py", ""),
        (
            "scripts/serve.py",
            "# /// script\n# dependencies = [\"transformers[serving]\", \"kernels\", \"boto3\"]\n# ///\nimport subprocess\nimport sys\ncmd = [\"transformers\", \"serve\", sys.argv[1]]\nsubprocess.run(cmd)\n",
        ),
    ]);
    let unused: Vec<_> = issue_keys(&json_issues(project.path(), &["--strict"]))
        .into_iter()
        .filter(|(code, _)| code == "CHK002")
        .collect();
    assert_eq!(
        unused,
        [(
            "CHK002".to_owned(),
            "script:scripts/serve.py:boto3".to_owned()
        )]
    );
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
fn binary_module_removed_in_py310_is_stdlib_for_a_py38_project() {
    // #940: `formatter` and `parser` ship in 3.8 and 3.9 only.
    let temp = tempfile::TempDir::new().expect("tempdir");
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"legacy\"\nversion = \"0.1.0\"\nrequires-python = \">=3.8\"\ndependencies = []\n\n[project.scripts]\nlegacy = \"legacy:main\"\n",
        ),
        (
            "legacy.py",
            "import sys\n\nif sys.version_info < (3, 10):\n    import formatter\n    import parser\n\n\ndef main() -> None:\n    pass\n",
        ),
    ] {
        fs::write(temp.path().join(file), text).expect("write fixture");
    }
    let keys = issue_keys(&json_issues(temp.path(), &[]));
    assert!(keys.is_empty(), "{keys:?}");
}

#[test]
fn binary_declared_backport_of_a_removed_stdlib_module_is_used() {
    // #939: PEP 594 backports provide the module on every target, whether
    // the target's stdlib still ships it or not.
    for requires in [">=3.9", ">=3.13"] {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let pyproject = format!(
            "[project]\nname = \"legacy\"\nversion = \"0.1.0\"\nrequires-python = \"{requires}\"\ndependencies = [\"legacy-cgi\", \"standard-imghdr\", \"audioop-lts\"]\n\n[project.scripts]\nlegacy = \"legacy:main\"\n"
        );
        fs::write(temp.path().join("pyproject.toml"), pyproject).expect("write fixture");
        fs::write(
            temp.path().join("legacy.py"),
            "import audioop\nimport cgi\nimport cgitb\nimport imghdr\n\n\ndef main() -> None:\n    pass\n",
        )
        .expect("write fixture");
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--strict", "--reporter", "json"])
            .arg(temp.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        // `standard-cgi` also provides `cgi`, but only `legacy-cgi` is declared.
        assert!(
            !stderr.contains("several distributions"),
            "{requires}: {stderr}"
        );
        let keys = issue_keys(&json_issues(temp.path(), &["--strict"]));
        assert!(keys.is_empty(), "{requires}: {keys:?}");
    }
}

#[test]
fn binary_invalid_egg_is_an_invalid_requirement() {
    // #929: pip 25 rejects these eggs, so they declare no
    // `requests;python-version<"3"` to report unused, and the egg's leading
    // name lowers the missing `requests` to a hint.
    let temp = tempfile::TempDir::new().expect("tempdir");
    fs::write(
        temp.path().join("requirements.txt"),
        "git+https://h/r.git#egg=requests;python_version<\"3\"\n-e \"git+https://h/r3.git#egg=with space\"\n",
    )
    .expect("write fixture");
    fs::write(temp.path().join("main.py"), "import requests\n").expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(temp.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("invalid requirement at `requirements.txt:1`")
            && stderr.contains("invalid requirement at `requirements.txt:2`")
            && stderr.contains("misplaced imports of `requests` are reported as hints"),
        "{stderr}"
    );
    assert!(issue_keys(&json_issues(temp.path(), &["--no-cache"])).is_empty());
    let strict = json_issues(temp.path(), &["--no-cache", "--strict"]);
    assert_eq!(
        issue_keys(&strict),
        [("CHK003".to_owned(), "main.py:requests".to_owned())]
    );
    assert_eq!(strict[0]["confidence"], "maybe");
}

#[test]
fn binary_marker_implied_by_requires_python_is_unconditional() {
    // #919: every interpreter `requires-python` allows satisfies the markers
    // of `foo` and `bar`, so CHK002 reports them; `baz` and `qux` may still
    // be skipped on a supported interpreter, so they stay conditional.
    let temp = tempfile::TempDir::new().expect("tempdir");
    fs::write(
        temp.path().join("pyproject.toml"),
        "[project]\nname = \"app\"\nversion = \"0\"\nrequires-python = \">=3.9\"\ndependencies = [\n  \"foo; python_version >= '3.8'\",\n  \"bar; python_full_version >= '3.9.0'\",\n  \"baz; python_version < '3.12'\",\n  \"qux; python_version >= '3.8' and sys_platform == 'linux'\",\n]\n",
    )
    .expect("write fixture");
    fs::write(temp.path().join("main.py"), "").expect("write fixture");
    let issues = json_issues(temp.path(), &["--no-cache"]);
    assert_eq!(
        issue_keys(&issues),
        [
            ("CHK002".to_owned(), "bar".to_owned()),
            ("CHK002".to_owned(), "foo".to_owned()),
        ]
    );
    assert!(issues.iter().all(|issue| issue["confidence"] == "certain"));

    // Poetry's `python` dependency bounds the interpreters the same way.
    let poetry = tempfile::TempDir::new().expect("tempdir");
    fs::write(
        poetry.path().join("pyproject.toml"),
        "[tool.poetry]\nname = \"app\"\nversion = \"0\"\n[tool.poetry.dependencies]\npython = \"^3.9\"\nfoo = { version = \"*\", python = \">=3.8\" }\nbar = { version = \"*\", python = \"<3.12\" }\n",
    )
    .expect("write fixture");
    fs::write(poetry.path().join("main.py"), "").expect("write fixture");
    assert_eq!(
        issue_keys(&json_issues(poetry.path(), &["--no-cache"])),
        [("CHK002".to_owned(), "foo".to_owned())]
    );
}

#[test]
fn binary_zensical_run_by_a_pdm_script_is_used() {
    // pdm runs its docs with `zensical serve`; the bundled binary map has to
    // name the CLI, or the declared doc dependency reads as unused.
    let temp = tempfile::TempDir::new().expect("tempdir");
    fs::write(
        temp.path().join("pyproject.toml"),
        "[project]\nname = \"app\"\nversion = \"0\"\n[dependency-groups]\ndoc = [\"zensical\"]\n[tool.pdm.scripts]\ndoc = { cmd = \"zensical serve\" }\n",
    )
    .expect("write fixture");
    fs::write(temp.path().join("main.py"), "").expect("write fixture");
    assert!(json_issues(temp.path(), &["--no-cache", "--strict"]).is_empty());
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

/// A workspace member's entry points into one of its files each keep their
/// object used (#884).
#[test]
fn member_entry_points_into_one_module_each_keep_their_object() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let files = [
        (
            "pyproject.toml",
            "[project]\nname = \"root\"\nversion = \"0.0.0\"\n\n[tool.uv.workspace]\nmembers = [\"svc\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        (
            "svc/pyproject.toml",
            "[project]\nname = \"tool\"\nversion = \"0.0.0\"\n\n[project.scripts]\na = \"tool.cli:main_a\"\nb = \"tool.cli:main_b\"\n",
        ),
        ("svc/src/tool/__init__.py", ""),
        (
            "svc/src/tool/cli.py",
            "def main_a():\n    pass\n\n\ndef main_b():\n    pass\n\n\ndef unnamed():\n    pass\n",
        ),
    ];
    for (file, text) in files {
        let path = temp.path().join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        fs::write(path, text).expect("write fixture");
    }

    let keys = issue_keys(&json_issues(temp.path(), &[]));
    let unused = |name: &str| {
        keys.iter().any(|(code, target)| {
            code == "CHK006" && target == &format!("svc/src/tool/cli.py:{name}")
        })
    };
    assert!(!unused("main_a"), "{keys:?}");
    assert!(!unused("main_b"), "{keys:?}");
    assert!(unused("unnamed"), "{keys:?}");
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

/// airflow's `stats.py` and `security/permissions.py` warn on import that
/// they are deprecated: shims kept for outside callers, so info (#778).
#[test]
fn binary_deprecated_shim_findings_are_info() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        (
            "acme/core.py",
            "from acme.permissions import ACTIONS\n\n\ndef run() -> int:\n    return len(ACTIONS)\n",
        ),
        (
            "acme/permissions.py",
            "import warnings\n\nwarnings.warn(\"moved\", DeprecationWarning, stacklevel=2)\n\nACTIONS = ()\nRESOURCE = \"r\"\n",
        ),
        (
            "acme/stats.py",
            "from warnings import warn\n\nwarn(\"moved\", category=DeprecationWarning)\n",
        ),
        ("acme/legacy.py", "LEGACY = 1\n"),
        (
            "tests/test_core.py",
            "from acme.core import run\n\n\ndef test_run() -> None:\n    assert run() == 0\n",
        ),
    ]);
    let severities: Vec<_> = json_issues(project.path(), &[])
        .iter()
        .filter(|issue| matches!(issue["code"].as_str(), Some("CHK001" | "CHK006")))
        .map(|issue| {
            format!(
                "{}:{}:{}",
                issue["code"].as_str().unwrap_or_default(),
                issue["target"].as_str().unwrap_or_default(),
                issue["severity"].as_str().unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(
        severities,
        [
            "CHK001:acme/legacy.py:warning",
            "CHK001:acme/stats.py:info",
            "CHK006:acme/permissions.py:RESOURCE:info",
        ],
        "{severities:?}"
    );
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

/// Under `--production` the evidence module star-imports a reachable module
/// left out of the evidence: what it binds is unknown, so a name read through
/// the star import still counts there (#803).
#[test]
fn binary_production_library_keeps_names_tests_read_through_a_star_import() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.chokkin]\nmode = \"library\"\n",
        ),
        (
            "acme/__init__.py",
            "from acme import core\n\n__all__ = [\"core\"]\n",
        ),
        (
            "acme/core.py",
            "def helper() -> None:\n    pass\n\n\ndef unused() -> None:\n    pass\n",
        ),
        ("acme/compat.py", "from acme.core import *\n"),
        (
            "tests/test_compat.py",
            "from acme.compat import helper\n\n\ndef test_helper() -> None:\n    helper()\n",
        ),
    ]);
    for extra in [&[][..], &["--production"][..]] {
        let keys = issue_keys(&json_issues(project.path(), extra));
        let flagged: Vec<_> = keys
            .iter()
            .filter(|(code, _)| code == "CHK006")
            .map(|(_, target)| target.as_str())
            .collect();
        // Without `--production` the tests reach the public `acme.compat`,
        // whose star import puts all of `acme.core` in the API.
        let expected: &[&str] = if extra.is_empty() {
            &[]
        } else {
            &["acme/core.py:unused"]
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

/// Every path that resolves outside the root says so, instead of "not found"
/// or nothing (#981): chokkin's own config is an error, a manifest's is a
/// warning naming what was not read.
#[test]
fn paths_outside_the_root_are_reported_as_such() {
    const PROJECT: &str =
        "[project]\nname = \"demo\"\nversion = \"0\"\ndependencies = [\"requests\"]\n";
    let cases: [(&str, &str, &str, u8); 6] = [
        (
            "requirements.txt",
            "-r ../x.txt\n",
            "requirements file `../x.txt` resolves outside the project root",
            2,
        ),
        (
            "requirements.txt",
            "-c ../x.txt\nrequests\n",
            "manifest: `../x.txt` (`-c` in `requirements.txt`) resolves outside the project root and was not read",
            0,
        ),
        (
            "pyproject.toml",
            "[tool.chokkin]\nproject = [\"../x\"]\n",
            "project[0]: path resolves outside the project root",
            2,
        ),
        (
            "pyproject.toml",
            "[tool.uv.sources]\nlib = { path = \"../x\" }\n",
            "manifest: `../x` (`tool.uv.sources.lib` in `pyproject.toml`) resolves outside the project root and was not read",
            0,
        ),
        (
            "pyproject.toml",
            "[tool.chokkin.workspaces.w]\npath = \"../x\"\n",
            "workspaces.w.path: path resolves outside the project root",
            2,
        ),
        (
            "pyproject.toml",
            "[tool.chokkin]\nentry = [\"/abs/path\"]\n",
            "entry[0]: path must be relative to the project root",
            2,
        ),
    ];
    for (file, text, expected, exit) in cases {
        let pyproject = if file == "pyproject.toml" {
            format!("{PROJECT}{text}")
        } else {
            PROJECT.to_owned()
        };
        let mut files = vec![
            ("x.txt", "requests\n"),
            ("x/pyproject.toml", PROJECT),
            ("p/demo/__init__.py", "import requests\n"),
            ("p/pyproject.toml", pyproject.as_str()),
        ];
        let requirements = format!("p/{file}");
        if file != "pyproject.toml" {
            files.push((requirements.as_str(), text));
        }
        let temp = write_project(&files);
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .arg("--no-cache")
            .arg(temp.path().join("p"))
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        assert!(stderr.contains(expected), "{text}: {stderr}");
        assert!(!stderr.contains("not found"), "{text}: {stderr}");
        assert!(!stderr.contains("missing constraints"), "{text}: {stderr}");
        assert_eq!(output.status.code(), Some(exit.into()), "{text}: {stderr}");
    }
}

/// A root `[tool.uv.sources]` path inside the root that is not a member says
/// why (#1000). Without a `[project]` table its code is analyzed as the
/// root's, but its own entries and declarations are not read: what it may
/// run or declare lowers the findings under it, and only those.
#[test]
fn binary_path_source_without_project_table_is_a_gap() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"app\"\nversion = \"0\"\ndependencies = [\"subpkg\", \"requests\", \"rich\", \"click\", \"attrs\"]\n\n[tool.uv.sources]\nsubpkg = { path = \"sub\" }\ngone = { path = \"missing-dir\" }\n",
        ),
        (
            "main.py",
            "import requests\nimport subpkg\nimport tomlkit\n",
        ),
        ("other.py", "import rich\n"),
        (
            "sub/setup.py",
            "from setuptools import setup\n\nsetup(name=\"subpkg\", install_requires=[\"httpx\"])\n",
        ),
        ("sub/subpkg/__init__.py", ""),
        ("sub/subpkg/__main__.py", "import httpx\n"),
        ("sub/subpkg/cli.py", "import rich\n"),
        ("sub/tools/gen.py", "import click\n"),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--confidence", "maybe", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    for expected in [
        "manifest: `sub` (`tool.uv.sources.subpkg` in `pyproject.toml`) has no `pyproject.toml` with a `[project]` table, so its code is analyzed as the root's without its own entries or dependencies",
        "manifest: `missing-dir` (`tool.uv.sources.gone` in `pyproject.toml`) does not exist",
    ] {
        assert!(stderr.contains(expected), "{stderr}");
    }
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let issues: BTreeSet<_> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|issue| {
            (
                issue["code"].as_str().unwrap_or_default().to_owned(),
                issue["target"].as_str().unwrap_or_default().to_owned(),
                issue["confidence"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let expected: BTreeSet<_> = [
        ("CHK001", "other.py", "certain"),
        ("CHK001", "sub/setup.py", "likely"),
        ("CHK001", "sub/subpkg/cli.py", "likely"),
        ("CHK001", "sub/tools/gen.py", "likely"),
        // `rich` is imported by an unreached root file too, but also under
        // the source, whose entries may reach it.
        ("CHK002", "rich", "likely"),
        ("CHK002", "click", "likely"),
        ("CHK002", "attrs", "certain"),
        ("CHK003", "main.py:tomlkit", "certain"),
        ("CHK003", "sub/subpkg/__main__.py:httpx", "maybe"),
    ]
    .into_iter()
    .map(|(code, target, confidence)| (code.to_owned(), target.to_owned(), confidence.to_owned()))
    .collect();
    assert_eq!(issues, expected, "{parsed}");

    for (target, reason) in [
        (
            "CHK002:click",
            "a file under a uv path source with no `[project]` table imports the distribution, and the source's entries were not read: `click` in `sub/tools/gen.py:1`",
        ),
        (
            "CHK001:sub/tools/gen.py",
            "a uv path source has no `[project]` table, so its entries and dependencies were not read: `sub`",
        ),
    ] {
        let explain = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--explain", target])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(explain.stderr).expect("utf8");
        assert!(stderr.contains(reason), "{target}: {stderr}");
    }
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

/// airflow's `scripts` member puts `ci/prek` on `sys.path` through its own
/// pytest `pythonpath`; it applies to that member's tests only (#775).
#[test]
fn binary_member_pytest_pythonpath_resolves_its_test_imports() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[tool.uv.workspace]\nmembers = [\"scripts\", \"task-sdk\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        (
            "scripts/pyproject.toml",
            "[project]\nname = \"acme-scripts\"\nversion = \"0.1.0\"\n\n[tool.pytest.ini_options]\npythonpath = [\".\", \"ci/prek\"]\n",
        ),
        (
            "scripts/ci/prek/check_imports.py",
            "def main() -> None:\n    pass\n",
        ),
        (
            "scripts/tests/ci/prek/test_check_imports.py",
            "from check_imports import main\n\n\ndef test_main() -> None:\n    main()\n",
        ),
        (
            "task-sdk/pyproject.toml",
            "[project]\nname = \"acme-task-sdk\"\nversion = \"0.1.0\"\n\n[tool.pytest]\npythonpath = [\"tests\"]\n",
        ),
        ("task-sdk/tests/task_sdk/__init__.py", ""),
        ("task-sdk/tests/task_sdk/helpers.py", "VALUE = 1\n"),
        (
            "task-sdk/tests/task_sdk/api/test_client.py",
            "from task_sdk.helpers import VALUE\n\n\ndef test_client() -> None:\n    assert VALUE\n",
        ),
        // Another member's `pythonpath` does not reach the root's tests.
        (
            "tests/test_root.py",
            "from check_imports import main\n\n\ndef test_root() -> None:\n    main()\n",
        ),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &[]));
    let unresolved: Vec<&str> = keys
        .iter()
        .filter(|(code, _)| code == "CHK010")
        .map(|(_, target)| target.as_str())
        .collect();
    assert_eq!(unresolved, ["tests/test_root.py:check_imports"], "{keys:?}");
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

/// transformers / flask / mlflow: a file's `sys.path` edit, or its conftest's
/// `monkeypatch.syspath_prepend`, puts a directory on the path (#719).
#[test]
fn binary_resolves_imports_from_directories_sys_path_edits_add() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        ("src/acme/__init__.py", ""),
        ("utils/check_docs.py", "def check() -> None:\n    pass\n"),
        (
            "tests/test_util.py",
            "import sys\nfrom pathlib import Path\n\nsys.path.insert(0, str(Path(__file__).parents[1] / \"utils\"))\n\nimport check_docs\nimport check_doc\n",
        ),
        (
            "tests/conftest.py",
            "import os\n\n\ndef test_apps(monkeypatch):\n    monkeypatch.syspath_prepend(os.path.join(os.path.dirname(__file__), \"test_apps\"))\n",
        ),
        ("tests/test_apps/cliapp/__init__.py", ""),
        (
            "tests/test_cli.py",
            "def test_cli(test_apps):\n    from cliapp import APP\n",
        ),
        // `tests/__init__.py` makes the root the basedir, so `"."` is needed.
        ("tests/__init__.py", ""),
        ("tests/integ/__init__.py", ""),
        (
            "tests/integ/conftest.py",
            "import os\n\n\ndef here(monkeypatch):\n    monkeypatch.chdir(os.path.dirname(__file__))\n    monkeypatch.syspath_prepend(\".\")\n",
        ),
        (
            "tests/integ/integ_helper.py",
            "def helper() -> None:\n    pass\n",
        ),
        (
            "tests/integ/test_integ.py",
            "from integ_helper import helper\n",
        ),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &[]));
    let unresolved: Vec<&str> = keys
        .iter()
        .filter(|(code, _)| code == "CHK010")
        .map(|(_, target)| target.as_str())
        .collect();
    assert_eq!(unresolved, ["tests/test_util.py:check_doc"], "{keys:?}");
}

/// Sphinx: `docs/conf.py` puts `../exts` on the path, so the extensions it
/// lists, appended ones included, resolve there and are used (#719).
#[test]
fn binary_sphinx_extensions_resolve_in_directories_conf_adds() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        (
            "docs/conf.py",
            "import os\nimport sys\n\nsys.path.insert(0, os.path.abspath(\"../exts\"))\nextensions = [\"ext_literal\"]\nextensions.append(\"ext_appended\")\n",
        ),
        ("exts/ext_literal.py", "def setup(app):\n    pass\n"),
        ("exts/ext_appended.py", "def setup(app):\n    pass\n"),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &[]));
    assert!(
        keys.iter()
            .all(|(code, _)| code != "CHK001" && code != "CHK010"),
        "{keys:?}"
    );
}

/// airflow: `conf.py` takes its extensions from a constants module, and that
/// module is what puts the extensions' directory on `sys.path` (#779).
#[test]
fn binary_sphinx_extensions_follow_an_imported_constants_module() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        (
            "docs/conf.py",
            "from common.constants import EXTS as BASE\n\nextensions = BASE\nextensions.append(\"ext_appended\")\n",
        ),
        (
            "src/common/constants.py",
            "import os\nimport sys\n\nfrom tools import ext_constant\n\nsys.path.append(os.path.join(os.path.dirname(__file__), \"exts\"))\nEXTS = [\"ext_constant\"]\n",
        ),
        (
            "src/common/exts/ext_constant.py",
            "def setup(app):\n    pass\n",
        ),
        (
            "src/common/exts/ext_appended.py",
            "def setup(app):\n    pass\n",
        ),
        (
            "src/common/exts/ext_unlisted.py",
            "def setup(app):\n    pass\n",
        ),
        ("src/tools/ext_constant.py", "def setup(app):\n    pass\n"),
    ]);
    let keys = issue_keys(&json_issues(project.path(), &[]));
    // Sphinx calls the `setup` of an extension found on the hinted path, not
    // that of a package module sharing its name.
    assert!(keys.iter().all(|(code, _)| code != "CHK010"), "{keys:?}");
    let unused_symbols: Vec<&str> = keys
        .iter()
        .filter(|(code, _)| code == "CHK006")
        .map(|(_, target)| target.as_str())
        .collect();
    assert_eq!(
        unused_symbols,
        ["src/tools/ext_constant.py:setup"],
        "{keys:?}"
    );
    let unused: Vec<&str> = keys
        .iter()
        .filter(|(code, _)| code == "CHK001")
        .map(|(_, target)| target.as_str())
        .collect();
    assert_eq!(unused, ["src/common/exts/ext_unlisted.py"], "{keys:?}");
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

/// A library member that declares no wheel targets ships its layout's
/// packages for CHK006/CHK007 only; that guess must not app-score its other
/// files the way declared targets do (#733).
#[test]
fn layout_shipped_library_member_keeps_library_scoring() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"root\"\nversion = \"0.1.0\"\n\n[tool.uv.workspace]\nmembers = [\"lib\", \"core\"]\n",
        ),
        (
            "lib/pyproject.toml",
            "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n\n[project]\nname = \"acme\"\nversion = \"0.1.0\"\n",
        ),
        ("lib/src/acme/__init__.py", ""),
        ("lib/scripts/release.py", "VERSION = \"1\"\n"),
        (
            "core/pyproject.toml",
            "[project]\nname = \"core\"\nversion = \"0.1.0\"\n\n[project.scripts]\ncore = \"core.main:main\"\n",
        ),
        ("core/src/core/__init__.py", ""),
        ("core/src/core/main.py", "def main():\n    pass\n"),
        ("core/src/core/orphan.py", ""),
    ]);
    let issues = json_issues(project.path(), &[]);
    assert_eq!(
        certain_chk001(&issues),
        ["core/src/core/orphan.py"],
        "{issues:?}"
    );
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
    // Sphinx reads the names the member's conf.py star-imports (#729).
    assert!(
        issues
            .iter()
            .all(|issue| issue["symbol"] != "docs.provider_conf:project"),
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

/// `--production` still parses a library's tests for CHK006, so what the
/// parser could not read there is reported like any other file (#894).
#[test]
fn binary_production_reports_parse_warnings_in_library_tests() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [tool.chokkin]\nmode = \"library\"\n",
        ),
        ("acme/__init__.py", ""),
        ("tests/test_api.py", "from acme import api\ndef broken(:\n"),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--production", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    let skipped = "parse: skipped `tests/test_api.py`: syntax error at line 2";
    assert!(stderr.contains(skipped), "{stderr}");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let diagnostics = parsed["diagnostics"].to_string();
    assert!(diagnostics.contains(skipped), "{diagnostics}");
}

/// A notebook whose JSON cannot be read is skipped like a syntax error: the
/// imports it hides may use any declared dependency (#893).
#[test]
fn binary_invalid_notebook_is_skipped_with_a_diagnostic() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/helper.py", ""),
        ("acme/demo.ipynb", "{\"cells\": 1}"),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.lines().any(|line| line
            == "parse: skipped `acme/demo.ipynb`: invalid notebook JSON: missing cells array"),
        "{stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let issues: Vec<_> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|issue| {
            (
                issue["code"].as_str().unwrap_or_default(),
                issue["target"].as_str().unwrap_or_default(),
                issue["confidence"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        issues,
        [
            ("CHK001", "acme/helper.py", "likely"),
            ("CHK002", "requests", "likely"),
        ],
        "{parsed}"
    );
}

/// A notebook code cell whose `source` is not text is dropped with a warning
/// that points at the notebook, and the other cells are still read (#898).
#[test]
fn binary_unreadable_notebook_cell_is_reported() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        (
            "acme/demo.ipynb",
            r#"{"cells": [{"cell_type": "code", "source": "import requests\n"}, {"cell_type": "code", "source": 1}]}"#,
        ),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "github"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    let warning = "parse: `acme/demo.ipynb`: some code cells were not read: their `cell_type` or `source` is not text";
    assert!(stderr.lines().any(|line| line == warning), "{stderr}");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(
        stdout
            .lines()
            .any(|line| line == format!("::warning file=acme/demo.ipynb,title=chokkin::{warning}")),
        "{stdout}"
    );
    assert!(!stdout.contains("CHK002"), "{stdout}");
}

/// A notebook with `IPython` magics and shell escapes is read, so the
/// dependencies its cells import are used, and nothing warns (#962).
#[test]
fn binary_notebook_with_ipython_syntax_is_read() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"pandas\", \"requests\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        (
            "acme/demo.ipynb",
            r#"{"cells": [{"cell_type": "code", "source": ["%pip install pandas\n", "!ls\n", "import pandas\n"]}, {"cell_type": "code", "source": "%%time\nimport requests\n"}]}"#,
        ),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "github"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(!stderr.contains("parse:"), "{stderr}");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(!stdout.contains("CHK002"), "{stdout}");
}

/// A dropped cell may import anything, so a dependency only it imports is not
/// certainly unused, nor is an orphan it may load (#902, ADR 0005).
#[test]
fn binary_unreadable_notebook_cell_lowers_absence_findings() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"pandas\", \"requests\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/helper.py", ""),
        (
            "acme/demo.ipynb",
            r#"{"cells": [{"cell_type": "code", "source": "import pandas\n"}, {"cell_type": "code", "source": ["import requests\n", 7]}]}"#,
        ),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let issues: Vec<_> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|issue| {
            (
                issue["code"].as_str().unwrap_or_default(),
                issue["target"].as_str().unwrap_or_default(),
                issue["confidence"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        issues,
        [
            ("CHK001", "acme/helper.py", "likely"),
            ("CHK002", "requests", "likely"),
        ],
        "{parsed}"
    );

    let explain = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--explain", "CHK002:requests"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(explain.stderr).expect("utf8");
    assert!(
        stderr.contains(
            "some code cells of a reachable notebook could not be read: `acme/demo.ipynb`"
        ),
        "{stderr}"
    );
}

/// A library caller may import a public module nothing in the package loads,
/// so the imports of one that could not be read are unknown: a dependency only
/// it may use is not certainly unused and `--fix` keeps it (#550, ADR 0005).
#[test]
fn binary_unread_public_module_of_a_library_lowers_unused_dependencies() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\", \"httpx\"]\n\n[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n\n[tool.chokkin]\nmode = \"library\"\n",
        ),
        ("src/acme/__init__.py", ""),
        ("src/acme/client.py", "import requests\n\ndef get(:\n"),
        ("src/acme/other.py", "import httpx\n"),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let issues: Vec<_> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .filter(|issue| issue["code"] == "CHK002")
        .map(|issue| {
            (
                issue["target"].as_str().unwrap_or_default(),
                issue["confidence"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(issues, [("requests", "likely")], "{parsed}");

    let explain = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--explain", "CHK002:requests"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(explain.stderr).expect("utf8");
    assert!(
        stderr.contains("a public module of the library could not be read: `src/acme/client.py`"),
        "{stderr}"
    );

    let fix = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--fix"])
        .arg(project.path())
        .output()
        .expect("run chokkin --fix");
    assert!(fix.status.code().is_some(), "{fix:?}");
    let manifest =
        std::fs::read_to_string(project.path().join("pyproject.toml")).expect("read manifest");
    assert!(manifest.contains("\"requests\""), "{manifest}");
}

/// CHK002 counts the imports of a library's public orphans, so an import it
/// cannot resolve in one may name the declared distribution, as in a
/// reachable module: the dependency is not certainly unused and `--fix`
/// keeps it (#942). A computed name lowers no distribution in either.
#[test]
fn binary_gaps_in_a_public_orphan_lower_unused_dependencies() {
    let cases = [
        (
            "import reqests\n",
            "likely",
            Some(
                "an import in a public module of the library could not be resolved to a distribution: `reqests` in `src/acme/client.py:1`",
            ),
        ),
        (
            "import importlib\n\nimportlib.import_module(NAME)\n",
            "certain",
            None,
        ),
    ];
    for (client, confidence, reason) in cases {
        let project = write_project(&[
            (
                "pyproject.toml",
                "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n\n[tool.hatch.build.targets.wheel]\npackages = [\"src/acme\"]\n\n[tool.chokkin]\nmode = \"library\"\n",
            ),
            ("src/acme/__init__.py", ""),
            ("src/acme/client.py", client),
            // Outside the wheel, so not public: its import lowers nothing.
            ("src/other/extra.py", "import reqests\n"),
        ]);

        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--reporter", "json"])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
        let issues: Vec<_> = parsed["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .filter(|issue| issue["code"] == "CHK002")
            .map(|issue| {
                (
                    issue["target"].as_str().unwrap_or_default(),
                    issue["confidence"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(issues, [("requests", confidence)], "{client}: {parsed}");
        let Some(reason) = reason else {
            continue;
        };

        let explain = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--explain", "CHK002:requests"])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(explain.stderr).expect("utf8");
        assert!(stderr.contains(reason), "{stderr}");

        let fix = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--fix"])
            .arg(project.path())
            .output()
            .expect("run chokkin --fix");
        assert!(fix.status.code().is_some(), "{fix:?}");
        let manifest =
            std::fs::read_to_string(project.path().join("pyproject.toml")).expect("read manifest");
        assert!(manifest.contains("\"requests\""), "{manifest}");
    }
}

/// A name computed at runtime may load any module, so an orphan is not
/// certainly unused; distributions stay certain, since the name still has to
/// be importable from a declared one (ADR 0005 §6).
#[test]
fn binary_opaque_dynamic_import_lowers_orphans_only() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[project.scripts]\nacme = \"acme.main:main\"\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/helper.py", ""),
        (
            "acme/main.py",
            "import importlib\nimport sys\n\ndef main():\n    importlib.import_module(sys.argv[1])\n",
        ),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let issues: Vec<_> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|issue| {
            (
                issue["code"].as_str().unwrap_or_default(),
                issue["target"].as_str().unwrap_or_default(),
                issue["confidence"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        issues,
        [
            ("CHK001", "acme/helper.py", "likely"),
            ("CHK002", "requests", "certain"),
        ],
        "{parsed}"
    );
    // The gap reaches the user without `--explain` (ADR 0005 §3, #909).
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert_eq!(stderr.matches("computed at runtime").count(), 1, "{stderr}");
    assert!(
        stderr.contains(
            "parse: `acme/main.py` imports a module by a name computed at runtime; unused files are reported below certain"
        ),
        "{stderr}"
    );
}

/// A reachable module with a syntax error is skipped like an undecodable one:
/// its imports are unknown, so what it may use is reported below certain,
/// `--fix` leaves the declaration alone, and stderr names the error.
#[test]
fn binary_syntax_error_source_is_skipped_with_a_diagnostic() {
    let pyproject = "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[project.scripts]\nacme = \"acme.main:main\"\n\n[tool.chokkin]\nmode = \"app\"\n";
    let project = write_project(&[
        ("pyproject.toml", pyproject),
        ("acme/__init__.py", ""),
        ("acme/helper.py", ""),
        (
            "acme/main.py",
            "import requests\nfrom acme import helper\ndef main(:\n    pass\n",
        ),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("parse: skipped `acme/main.py`: syntax error at line 3"),
        "{stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let issues: Vec<_> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|issue| {
            (
                issue["code"].as_str().unwrap_or_default(),
                issue["target"].as_str().unwrap_or_default(),
                issue["confidence"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        issues,
        [
            ("CHK001", "acme/helper.py", "likely"),
            ("CHK002", "requests", "likely"),
        ],
        "{parsed}"
    );

    let status = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--fix"])
        .arg(project.path())
        .status()
        .expect("run chokkin --fix");
    assert!(status.code().is_some(), "{status:?}");
    assert_eq!(
        fs::read_to_string(project.path().join("pyproject.toml")).expect("read pyproject"),
        pyproject
    );
}

/// The default severity of `code`: an unused file is a warning (spec §3).
fn severity(code: &str) -> &'static str {
    if code == "CHK001" { "warning" } else { "error" }
}

/// `(code, target, severity, confidence)` of each issue in `root`.
fn issue_levels(root: &std::path::Path) -> Vec<(String, String, String, String)> {
    json_issues(root, &["--no-cache"])
        .iter()
        .map(|issue| {
            let field = |key: &str| issue[key].as_str().unwrap_or_default().to_owned();
            (
                field("code"),
                field("target"),
                field("severity"),
                field("confidence"),
            )
        })
        .collect()
}

/// An import no source resolves may come from any declared distribution, so
/// those reported unused fall to likely and `--fix` keeps them; CHK010 is
/// unchanged, and an import that never fails lowers nothing (#911).
#[test]
fn binary_unresolved_import_lowers_unused_distributions() {
    let pyproject = "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"acme-widgets-sdk\"]\n\n[project.scripts]\nacme = \"acme.main:main\"\n\n[tool.chokkin]\nmode = \"app\"\n";
    let level = |code: &str, target: &str, severity: &str, confidence: &str| {
        (
            code.to_owned(),
            target.to_owned(),
            severity.to_owned(),
            confidence.to_owned(),
        )
    };
    let project = write_project(&[
        ("pyproject.toml", pyproject),
        ("acme/__init__.py", ""),
        (
            "acme/main.py",
            "import zz_widgets_core\n\ndef main():\n    zz_widgets_core.run()\n",
        ),
    ]);
    assert_eq!(
        issue_levels(project.path()),
        [
            level("CHK002", "acme-widgets-sdk", "error", "likely"),
            level(
                "CHK010",
                "acme/main.py:zz_widgets_core",
                "warning",
                "likely"
            ),
        ]
    );

    let explain = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--explain", "CHK002:acme-widgets-sdk"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(explain.stderr).expect("utf8");
    assert!(
        stderr.contains(
            "a reachable import could not be resolved to a distribution: `zz_widgets_core` in `acme/main.py:1`"
        ),
        "{stderr}"
    );

    let status = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--fix"])
        .arg(project.path())
        .status()
        .expect("run chokkin --fix");
    assert!(status.code().is_some(), "{status:?}");
    assert_eq!(
        fs::read_to_string(project.path().join("pyproject.toml")).expect("read pyproject"),
        pyproject
    );

    // An optional import falls back when missing, so it names no dependency.
    let optional = write_project(&[
        ("pyproject.toml", pyproject),
        ("acme/__init__.py", ""),
        (
            "acme/main.py",
            "try:\n    import zz_widgets_core\nexcept ImportError:\n    pass\n\ndef main():\n    pass\n",
        ),
    ]);
    assert_eq!(
        issue_levels(optional.path()),
        [
            level("CHK002", "acme-widgets-sdk", "error", "certain"),
            level("CHK010", "acme/main.py:zz_widgets_core", "info", "likely"),
        ]
    );
}

/// A PEP 723 script installs its own block, so an import it cannot resolve
/// lowers that block's unused entries and leaves the project's alone (#911).
#[test]
fn binary_unresolved_script_import_lowers_only_its_block() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[project.scripts]\nacme = \"acme.main:main\"\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/main.py", "def main():\n    pass\n"),
        (
            "scripts/report.py",
            "# /// script\n# dependencies = [\"rich\"]\n# ///\nimport zz_render_core\n",
        ),
    ]);
    let found: Vec<_> = issue_levels(project.path())
        .into_iter()
        .filter(|(code, ..)| code == "CHK002")
        .map(|(_, target, _, confidence)| (target, confidence))
        .collect();
    assert_eq!(
        found,
        [
            ("requests".to_owned(), "certain".to_owned()),
            (
                "script:scripts/report.py:rich".to_owned(),
                "likely".to_owned()
            ),
        ]
    );
}

const COMMAND_PYPROJECT: &str = "[project]\nname = \"mypkg\"\nversion = \"0.1.0\"\n\n[project.scripts]\nmypkg = \"mypkg.main:main\"\n\n[tool.chokkin]\nmode = \"app\"\n";

/// Scripts and `-m` modules task files and tool configs run are entries,
/// so what they import is followed and their commands are no gap (#912).
/// They are dev roots (#797): `--production` holds them back, and the files
/// they reach are unread code again (#913).
#[test]
fn binary_command_run_files_are_entries() {
    let mut files = vec![
        ("pyproject.toml", COMMAND_PYPROJECT),
        ("mypkg/__init__.py", ""),
        ("mypkg/main.py", "def main():\n    pass\n"),
        (
            "Makefile",
            "gen:\n\tpython mypkg/gen.py\n\nmod:\n\tpython -m mypkg.modrun\n",
        ),
        (
            "tox.ini",
            "[testenv:gen]\ncommands = python mypkg/toxgen.py\n",
        ),
        (
            ".pre-commit-config.yaml",
            "repos:\n  - repo: local\n    hooks:\n      - id: ghgen\n        name: ghgen\n        entry: python -m mypkg.ghgen\n        language: system\n",
        ),
        ("Procfile", "web: gunicorn mypkg.server:app\n"),
        ("mypkg/gen.py", "from mypkg import genhelper\n"),
        ("mypkg/genhelper.py", ""),
        ("mypkg/orphan.py", ""),
    ];
    for path in [
        "mypkg/modrun.py",
        "mypkg/toxgen.py",
        "mypkg/ghgen.py",
        "mypkg/server.py",
    ] {
        files.push((path, "app = None\n"));
    }
    let project = write_project(&files);
    // `certain` on the orphan shows no command is a gap.
    let unused: Vec<_> = issue_levels(project.path())
        .into_iter()
        .filter(|(code, ..)| code == "CHK001")
        .collect();
    assert_eq!(
        unused,
        [(
            "CHK001".to_owned(),
            "mypkg/orphan.py".to_owned(),
            "warning".to_owned(),
            "certain".to_owned()
        )]
    );
    let trace = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--trace", "mypkg/orphan.py"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(trace.stderr).expect("utf8");
    for root in [
        "mypkg/ghgen.py (command: pre-commit entry (.pre-commit-config.yaml:6))",
        "mypkg/toxgen.py (command: tox commands (tox.ini:2))",
        "mypkg/modrun.py (command: make recipe (Makefile:5))",
    ] {
        assert!(stderr.contains(root), "{stderr}");
    }

    let production: Vec<_> = json_issues(project.path(), &["--no-cache", "--production"])
        .iter()
        .filter(|issue| issue["code"] == "CHK001")
        .map(|issue| {
            (
                issue["target"].as_str().unwrap_or_default().to_owned(),
                issue["confidence"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let likely = |path: &str| (path.to_owned(), "likely".to_owned());
    assert_eq!(
        production,
        [
            likely("mypkg/gen.py"),
            likely("mypkg/genhelper.py"),
            likely("mypkg/ghgen.py"),
            likely("mypkg/modrun.py"),
            likely("mypkg/orphan.py"),
            likely("mypkg/toxgen.py"),
        ]
    );
}

/// A module `-c` code imports, a task runner loads, or a relocated `-m` runs
/// is no entry, so one no entry reaches lowers unused files to likely and is
/// warned once per file (#913).
#[test]
fn binary_unread_command_target_lowers_unused_files() {
    let project = write_project(&[
        ("pyproject.toml", COMMAND_PYPROJECT),
        ("mypkg/__init__.py", ""),
        ("mypkg/main.py", "def main():\n    pass\n"),
        ("mypkg/cimp.py", ""),
        ("mypkg/relmod.py", ""),
        ("mypkg/tasks.py", ""),
        (
            "Makefile",
            "gen:\n\tpython -c \"import mypkg.cimp\"\n\tcd sub && python -m mypkg.relmod\n\tcelery -A mypkg.tasks worker\n",
        ),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let unused: Vec<_> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .filter(|issue| issue["code"] == "CHK001")
        .map(|issue| {
            (
                issue["target"].as_str().unwrap_or_default().to_owned(),
                issue["confidence"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let likely = |path: &str| (path.to_owned(), "likely".to_owned());
    assert_eq!(
        unused,
        [
            likely("mypkg/cimp.py"),
            likely("mypkg/relmod.py"),
            likely("mypkg/tasks.py"),
        ],
        "{parsed}"
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains(
            "plugin: `Makefile:2` runs `mypkg.cimp` and 2 more targets as Python code no entry reaches; unused files are reported below certain"
        ),
        "{stderr}"
    );

    let explain = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--explain", "CHK001:mypkg/cimp.py"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(explain.stderr).expect("utf8");
    assert!(
        stderr.contains(
            "a command runs Python code no entry reaches: `mypkg.cimp` in `Makefile:2` (and 2 more)"
        ),
        "{stderr}"
    );
}

/// What a Dockerfile or Procfile runs is deployed, so it keeps its own
/// context and stays a root under `--production`, while a script a Makefile
/// runs is dev: its dev-group import is not misplaced (#912).
#[test]
fn binary_deploy_command_runs_keep_runtime_context() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\", \"rich\"]\n\n[dependency-groups]\ndev = [\"click\"]\n\n[tool.chokkin]\nmode = \"app\"\nproject = [\"*.py\"]\n",
        ),
        (
            "Dockerfile",
            "FROM python:3.12\nCMD [\"python\", \"run.py\"]\n",
        ),
        ("Procfile", "worker: python worker.py\n"),
        ("Makefile", "gen:\n\tpython gen.py\n"),
        ("run.py", "import requests\n"),
        ("worker.py", "import rich\n"),
        ("gen.py", "import click\n"),
    ]);
    for mode in [&["--strict"][..], &["--strict", "--production"]] {
        let keys = issue_keys(&json_issues(project.path(), mode));
        assert!(keys.is_empty(), "{mode:?}: {keys:?}");
    }
}

/// A file with a `__main__` block or one the wheel ships stays a production
/// root when a task file also runs it: `make run` runs the flat app and tox
/// runs the library's module, neither of which is a dev script (#912).
#[test]
fn binary_command_runs_keep_main_block_and_shipped_files() {
    let app = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[tool.chokkin]\nmode = \"app\"\nproject = [\"*.py\"]\n",
        ),
        ("Makefile", "run:\n\tpython bot.py\n"),
        (
            "bot.py",
            "import helpers\n\nif __name__ == \"__main__\":\n    helpers.run()\n",
        ),
        (
            "helpers.py",
            "import requests\n\n\ndef run():\n    requests.get\n",
        ),
    ]);
    let library = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"mymod\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[tool.setuptools]\npy-modules = [\"mymod\"]\n\n[tool.chokkin]\nmode = \"library\"\n",
        ),
        ("tox.ini", "[testenv]\ncommands = python -m mymod\n"),
        ("mymod.py", "import requests\n"),
    ]);
    for project in [&app, &library] {
        let keys = issue_keys(&json_issues(project.path(), &["--production"]));
        assert!(keys.iter().all(|(code, _)| code == "CHK008"), "{keys:?}");
    }
}

/// A computed target may run any file, so it lowers unused files (#913).
#[test]
fn binary_computed_command_target_lowers_unused_files() {
    let computed = write_project(&[
        ("pyproject.toml", COMMAND_PYPROJECT),
        ("mypkg/__init__.py", ""),
        ("mypkg/main.py", "def main():\n    pass\n"),
        ("mypkg/orphan.py", ""),
        ("Makefile", "gen:\n\t$(PYTHON) $(SCRIPT)\n"),
    ]);
    assert_eq!(
        issue_levels(computed.path()),
        [(
            "CHK001".to_owned(),
            "mypkg/orphan.py".to_owned(),
            "warning".to_owned(),
            "likely".to_owned()
        )]
    );
}

/// A typo names no file, and third-party and reached targets hide nothing
/// (#913).
#[test]
fn binary_unresolved_command_target_keeps_unused_files_certain() {
    let typo = write_project(&[
        ("pyproject.toml", COMMAND_PYPROJECT),
        ("mypkg/__init__.py", ""),
        ("mypkg/main.py", "def main():\n    pass\n"),
        ("mypkg/orphan.py", ""),
        (
            "Makefile",
            "gen:\n\tpython mypkg/gne.py\n\tpython -m json.tool a.json\n\tpython -m mypkg.main\n\tpython mypkg/main.py\n",
        ),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache"])
        .arg(typo.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(!stderr.contains("no entry reaches"), "{stderr}");
    assert_eq!(
        issue_levels(typo.path()),
        [(
            "CHK001".to_owned(),
            "mypkg/orphan.py".to_owned(),
            "warning".to_owned(),
            "certain".to_owned()
        )]
    );
}

/// Acceptance fixture 3 of #888: a settings package chosen by assigning
/// `DJANGO_SETTINGS_MODULE` in `manage.py`, whose module star-imports the
/// fields the plugin reads (#914).
fn django_settings_package(manage: &str) -> tempfile::TempDir {
    write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"mysite\"\nversion = \"0.1.0\"\ndependencies = [\"django\"]\n",
        ),
        ("manage.py", manage),
        ("mysite/__init__.py", ""),
        ("mysite/settings/__init__.py", ""),
        (
            "mysite/settings/base.py",
            "ROOT_URLCONF = \"mysite.urls\"\nINSTALLED_APPS = [\"django.contrib.admin\"]\n",
        ),
        ("mysite/settings/dev.py", "from .base import *\n"),
        // No public name: Django reading `urlpatterns` is not what this checks.
        ("mysite/urls.py", "# URLconf\n"),
    ])
}

#[test]
fn binary_django_import_settings_reach_files_in_production() {
    for setting in [
        "AUTHENTICATION_BACKENDS = ['mysite.backend.Backend']",
        "PASSWORD_HASHERS = ['mysite.backend.Hasher']",
        "DATABASE_ROUTERS = ('mysite.backend.Router',)",
        "STORAGES = {'default': {'BACKEND': 'mysite.backend.Storage'}}",
        "STORAGES = {}\nSTORAGES['default'] = {'BACKEND': 'mysite.backend.Storage'}",
        "DEFAULT_FILE_STORAGE = 'mysite.backend.Storage'",
        "TEMPLATES = [{'OPTIONS': {'context_processors': ['mysite.backend.processor']}}]",
        "LOGGING = {'handlers': {'custom': {'class': 'mysite.backend.Handler'}}}",
        "BUILTIN_PLUGINS = ['mysite.backend.Plugin']",
        "CELERY_BROKER_TRANSPORT = 'mysite.backend.Transport'",
        "PRIVATE_FILE_STORAGE = 'mysite.backend.Storage'",
        "CUSTOM_MODULE = 'mysite.backend'",
        "WSGI_APPLICATION = 'mysite.backend.application'",
        "ASGI_APPLICATION = 'mysite.backend.application'",
    ] {
        let settings = format!("INSTALLED_APPS = []\n{setting}\n");
        let project = write_project(&[
            (
                "pyproject.toml",
                "[project]\nname = 'mysite'\nversion = '0'\ndependencies = ['django']\n\n[tool.chokkin]\nmode = 'app'\n",
            ),
            (
                "manage.py",
                "import os\nos.environ.setdefault('DJANGO_SETTINGS_MODULE', 'mysite.settings')\n",
            ),
            ("mysite/__init__.py", ""),
            ("mysite/settings.py", &settings),
            ("mysite/backend.py", "from . import helper\n"),
            ("mysite/helper.py", ""),
            ("mysite/unused.py", ""),
        ]);
        let issues = json_issues(project.path(), &["--production", "--no-cache"]);
        let unused: Vec<_> = issues
            .iter()
            .filter(|issue| issue["code"] == "CHK001")
            .map(|issue| issue["path"].as_str().expect("path"))
            .collect();
        assert_eq!(unused, ["mysite/unused.py"], "{setting}");
        let trace = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--production", "--no-cache", "--trace", "mysite/backend.py"])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(trace.stderr).expect("utf8");
        let field = setting.split_whitespace().next().expect("field");
        let evidence = if matches!(field, "WSGI_APPLICATION" | "ASGI_APPLICATION") {
            "file mysite/backend.py"
        } else {
            field
        };
        assert!(stderr.contains(evidence), "{setting}: {stderr}");
    }
}

/// An installed app in the shape of Django's tutorial: what Django loads by
/// convention from it, and what it reads by name there, is used (#399).
#[allow(clippy::too_many_lines)]
fn django_app_project(installed: &str) -> tempfile::TempDir {
    let settings = format!("INSTALLED_APPS = {installed}\nROOT_URLCONF = 'mysite.urls'\n");
    write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = 'mysite'\nversion = '0'\ndependencies = ['django']\n\n[tool.chokkin]\nmode = 'app'\n",
        ),
        (
            "manage.py",
            "import os\nos.environ.setdefault('DJANGO_SETTINGS_MODULE', 'mysite.settings')\n",
        ),
        ("mysite/__init__.py", ""),
        ("mysite/settings.py", &settings),
        ("mysite/urls.py", "# URLconf\n"),
        (
            "mysite/apps.py",
            "from django.contrib.admin.apps import AdminConfig\n\n\nclass SiteAdminConfig(AdminConfig):\n    default_site = 'mysite.sites.Site'\n",
        ),
        ("mysite/models.py", ""),
        ("mysite/polls/__init__.py", ""),
        (
            "mysite/polls/apps.py",
            "from django import apps\n\n\nclass Unused:\n    name = 'elsewhere'\n\n\nclass PollsConfig(apps.AppConfig):\n    name = 'mysite.polls'\n\n    def ready(self):\n        from . import signals, checks\n",
        ),
        (
            "mysite/polls/models.py",
            "from django.db import models\n\n\nclass Question(models.Model):\n    pass\n",
        ),
        (
            "mysite/polls/admin.py",
            "from django.contrib import admin\n\nfrom .models import Question\n\n\n@admin.register(Question)\nclass QuestionAdmin(admin.ModelAdmin):\n    pass\n",
        ),
        (
            "mysite/polls/signals.py",
            "from django.db.models.signals import post_save\nfrom django.dispatch import receiver\n\n\n@receiver(post_save)\ndef on_save(sender, **kwargs):\n    pass\n",
        ),
        (
            "mysite/polls/checks.py",
            "from django.core import checks\n\n\n@checks.register()\ndef check_polls(app_configs, **kwargs):\n    return []\n",
        ),
        ("mysite/polls/templatetags/__init__.py", ""),
        (
            "mysite/polls/templatetags/poll_extras.py",
            "from django import template\n\nregister = template.Library()\n\n\n@register.filter\ndef shout(value):\n    return value.upper()\n\n\ndef dead():\n    pass\n",
        ),
        // Intentionally unused: no `register`, so Django never loads it.
        (
            "mysite/polls/templatetags/helpers.py",
            "def helper():\n    pass\n",
        ),
        (
            "mysite/polls/templatetags/extra/__init__.py",
            "from django import template\n\nregister = template.Library()\n\n\n@register.simple_tag\ndef now():\n    pass\n\n\ndef dead():\n    pass\n",
        ),
        ("mysite/polls/management/__init__.py", ""),
        ("mysite/polls/management/commands/__init__.py", ""),
        (
            "mysite/polls/management/commands/closepoll.py",
            "from django.core.management.base import BaseCommand\n\n\nclass Command(BaseCommand):\n    def handle(self, *args, **options):\n        pass\n",
        ),
        (
            "mysite/polls/management/commands/notacommand.py",
            "def run():\n    pass\n",
        ),
        (
            "mysite/polls/management/commands/_shared.py",
            "class Command:\n    pass\n",
        ),
        // Commands that bind `Command` by import, and ones that do not.
        (
            "mysite/polls/management/commands/imported.py",
            "from mysite.polls.base import Command\n",
        ),
        (
            "mysite/polls/management/commands/aliased.py",
            "from mysite.polls.base import Base as Command\n",
        ),
        (
            "mysite/polls/management/commands/starred.py",
            "from mysite.polls.base import *\n",
        ),
        (
            "mysite/polls/management/commands/renamed.py",
            "from mysite.polls.base import Command as Base\n",
        ),
        (
            "mysite/polls/management/commands/deferred.py",
            "def load():\n    from mysite.polls.base import Command\n",
        ),
        (
            "mysite/polls/management/commands/unrelated.py",
            "from mysite.polls.base import Base\n",
        ),
        (
            "mysite/polls/base.py",
            "class Base:\n    pass\n\n\nCommand = Base\n",
        ),
        // Not under `templatetags/`, so no library.
        (
            "mysite/polls/library.py",
            "from django import template\n\nregister = template.Library()\n",
        ),
        ("mysite/polls/orphan.py", ""),
    ])
}

#[test]
fn binary_django_installed_app_conventions_are_used() {
    let flagged = |installed: &str| {
        let project = django_app_project(installed);
        let mut keys: Vec<String> = issue_keys(&json_issues(
            project.path(),
            &["--production", "--no-cache"],
        ))
        .into_iter()
        .filter(|(code, target)| {
            matches!(code.as_str(), "CHK001" | "CHK006") && target.starts_with("mysite/polls/")
        })
        .map(|(code, target)| format!("{code}:{target}"))
        .collect();
        keys.sort();
        keys
    };
    let unused = [
        "CHK001:mysite/polls/library.py",
        "CHK001:mysite/polls/management/commands/_shared.py",
        "CHK001:mysite/polls/management/commands/deferred.py",
        "CHK001:mysite/polls/management/commands/notacommand.py",
        "CHK001:mysite/polls/management/commands/renamed.py",
        "CHK001:mysite/polls/management/commands/unrelated.py",
        "CHK001:mysite/polls/orphan.py",
        "CHK001:mysite/polls/templatetags/helpers.py",
        "CHK006:mysite/polls/apps.py:Unused",
        "CHK006:mysite/polls/templatetags/extra/__init__.py:dead",
        "CHK006:mysite/polls/templatetags/poll_extras.py:dead",
    ];
    for installed in [
        "['django.contrib.admin', 'mysite.polls']",
        "['django.contrib.admin.apps.AdminConfig', 'mysite.polls.apps.PollsConfig']",
        // A config with no `name` inherits the admin's.
        "['mysite.apps.SiteAdminConfig', 'mysite.polls']",
        // An `INSTALLED_APPS` not read in full may install the admin (ADR 0005).
        "['mysite.polls', *EXTRA_APPS]",
        "['mysite.polls']\nINSTALLED_APPS += ['django.contrib.admin']",
    ] {
        assert_eq!(flagged(installed), unused, "{installed}");
    }
    // Without the admin, nothing autodiscovers `admin.py`, but Django still
    // imports `models.py`.
    let mut without_admin =
        flagged("['django.contrib.admin.apps.SimpleAdminConfig', 'mysite.polls']");
    without_admin.retain(|key| {
        key.starts_with("CHK001") && (key.contains("admin.py") || key.contains("models.py"))
    });
    assert_eq!(without_admin, ["CHK001:mysite/polls/admin.py"]);
    // The inherited config makes the package that defines it no app.
    let project = django_app_project("['mysite.apps.SiteAdminConfig', 'mysite.polls']");
    let keys = issue_keys(&json_issues(
        project.path(),
        &["--production", "--no-cache"],
    ));
    assert!(
        keys.contains(&("CHK001".to_owned(), "mysite/models.py".to_owned())),
        "{keys:?}"
    );
    assert!(
        !keys
            .iter()
            .any(|(_, target)| target.starts_with("mysite/apps.py")),
        "{keys:?}"
    );
}

#[test]
fn binary_django_trace_names_the_convention() {
    let project = django_app_project("['mysite.polls']");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args([
            "--production",
            "--no-cache",
            "--trace",
            "mysite/polls/templatetags/poll_extras.py",
        ])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("(Django templatetags)"), "{stderr}");
}

#[test]
fn binary_django_receivers_outside_installed_apps_are_used() {
    // netbox connects receivers from `netbox/search/signals.py`, a module of
    // the project package that is no installed app; a reusable app has no
    // settings at all.
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = 'mysite'\nversion = '0'\ndependencies = ['django']\n\n[tool.chokkin]\nmode = 'app'\nentry = ['mysite/main.py']\n",
        ),
        ("mysite/__init__.py", ""),
        ("mysite/main.py", "from . import hooks\n"),
        (
            "mysite/hooks.py",
            "from django.db.models.signals import post_save\nfrom django.dispatch import receiver\n\n\n@receiver(post_save)\ndef on_save(sender, **kwargs):\n    pass\n\n\ndef dead():\n    pass\n",
        ),
    ]);
    let flagged: Vec<String> = issue_keys(&json_issues(project.path(), &["--no-cache"]))
        .into_iter()
        .filter(|(code, target)| code == "CHK006" && target.starts_with("mysite/hooks.py"))
        .map(|(_, target)| target)
        .collect();
    assert_eq!(flagged, ["mysite/hooks.py:dead"]);
}

#[test]
fn binary_django_import_settings_explain_the_setting_line() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = 'mysite'\nversion = '0'\ndependencies = ['django']\n",
        ),
        ("mysite/__init__.py", ""),
        (
            "mysite/settings.py",
            "INSTALLED_APPS = []\nAUTHENTICATION_BACKENDS = [\n    'unknown_backend.Custom',\n]\n",
        ),
    ]);
    let issues = json_issues(project.path(), &["--no-cache"]);
    let issue = issues
        .iter()
        .find(|issue| issue["code"] == "CHK010")
        .expect("unresolved backend");
    assert_eq!(issue["path"], "mysite/settings.py");
    assert_eq!(issue["line"], 3);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args([
            "--no-cache",
            "--explain",
            "CHK010:mysite/settings.py:unknown_backend",
        ])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("AUTHENTICATION_BACKENDS"), "{stderr}");
    assert!(stderr.contains("mysite/settings.py:3"), "{stderr}");
}

#[test]
fn binary_django_computed_import_settings_lower_unused_findings() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = 'mysite'\nversion = '0'\ndependencies = ['django']\n\n[tool.chokkin]\nmode = 'app'\n",
        ),
        ("mysite/__init__.py", ""),
        (
            "mysite/settings.py",
            "INSTALLED_APPS = []\nAUTHENTICATION_BACKENDS = build_backends()\n",
        ),
        ("mysite/backend.py", ""),
    ]);
    let issues = json_issues(project.path(), &["--production", "--no-cache"]);
    let unused = issues
        .iter()
        .find(|issue| issue["code"] == "CHK001")
        .expect("unused backend");
    assert_ne!(unused["confidence"], "certain");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args([
            "--production",
            "--no-cache",
            "--explain",
            "CHK001:mysite/backend.py",
        ])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("AUTHENTICATION_BACKENDS"), "{stderr}");
    assert!(stderr.contains("mysite/settings.py:2"), "{stderr}");
}

/// A settings module named by `DJANGO_SETTINGS_MODULE` is read wherever it
/// lives, assigned or `setdefault`, and its star imports supply the fields
/// (#914).
#[test]
fn binary_django_settings_module_reads_a_settings_package() {
    for manage in [
        "import os\n\nos.environ[\"DJANGO_SETTINGS_MODULE\"] = \"mysite.settings.dev\"\n",
        "import os\n\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"mysite.settings.dev\")\n",
    ] {
        let project = django_settings_package(manage);
        assert_eq!(issue_levels(project.path()), [], "{manage}");
    }
}

/// Django puts the directory of `manage.py` on `sys.path`, and a `wsgi.py`
/// sits in the package beside it, so a nested setter's settings module and
/// URL configuration are looked up from there before any other `settings.py`
/// is guessed (#914).
#[test]
fn binary_django_settings_module_resolves_beside_a_nested_setter() {
    for (setter, contents) in [
        (
            "demo/site/manage.py",
            "import os\n\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"site.settings\")\n",
        ),
        (
            "demo/site/site/wsgi.py",
            "import os\n\nos.environ[\"DJANGO_SETTINGS_MODULE\"] = \"site.settings\"\n",
        ),
    ] {
        let project = write_project(&[
            (
                "pyproject.toml",
                "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.chokkin]\nmode = \"app\"\n",
            ),
            (setter, contents),
            ("demo/site/__init__.py", ""),
            ("demo/site/site/__init__.py", ""),
            (
                "demo/site/site/settings.py",
                "ROOT_URLCONF = \"site.urls\"\n",
            ),
            ("demo/site/site/urls.py", "# URL configuration\n"),
            ("other/settings.py", ""),
        ]);
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--reporter", "json"])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        assert!(!stderr.contains("ambiguous Django settings"), "{stderr}");
        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
        let unused: Vec<_> = parsed["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .filter(|issue| issue["code"] == "CHK001")
            .filter_map(|issue| issue["target"].as_str())
            .collect();
        assert!(
            !unused.contains(&"demo/site/site/settings.py")
                && !unused.contains(&"demo/site/site/urls.py")
                && unused.contains(&"other/settings.py"),
            "{setter}: {unused:?}"
        );
    }
}

/// Settings that cannot be found leave the Django project's files and
/// distributions unexplained: they fall to likely, `--fix` keeps them, and a
/// warning says why (#914).
#[test]
fn binary_unread_django_settings_lower_unused_files_and_distributions() {
    let level = |code: &str, target: &str| {
        (
            code.to_owned(),
            target.to_owned(),
            severity(code).to_owned(),
            "likely".to_owned(),
        )
    };
    let cases = [
        (
            "import os\n\nos.environ[\"DJANGO_SETTINGS_MODULE\"] = settings_module()\n",
            "plugin: django: `manage.py:3` sets DJANGO_SETTINGS_MODULE in a form that could not be read, and no settings.py was found; unused files and dependencies are reported below certain",
            "the Django settings could not be found or read: `DJANGO_SETTINGS_MODULE` in `manage.py:3`",
        ),
        (
            "import os\n\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"mysite.settings.prod\")\n",
            "plugin: django: settings module `mysite.settings.prod` set in `manage.py:3` is not a project file; unused files and dependencies are reported below certain",
            "the Django settings could not be found or read: `mysite.settings.prod` in `manage.py:3`",
        ),
    ];
    for (manage, warning, explain) in cases {
        let project = django_settings_package(manage);
        // `ROOT_URLCONF = "mysite.urls"` in the unread settings names the
        // URLconf by its dotted name, so that file falls to info (#1026).
        assert_eq!(
            issue_levels(project.path()),
            [
                level("CHK001", "mysite/settings/base.py"),
                level("CHK001", "mysite/settings/dev.py"),
                (
                    "CHK001".to_owned(),
                    "mysite/urls.py".to_owned(),
                    "info".to_owned(),
                    "likely".to_owned(),
                ),
                level("CHK002", "django"),
            ],
            "{manage}"
        );
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--explain", "CHK002:django"])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        assert!(stderr.contains(warning), "{stderr}");
        assert!(stderr.contains(explain), "{stderr}");
    }

    // A star import the plugin cannot follow may set what it reads.
    let project = django_settings_package(
        "import os\n\nos.environ[\"DJANGO_SETTINGS_MODULE\"] = \"mysite.settings.dev\"\n",
    );
    fs::write(
        project.path().join("mysite/settings/dev.py"),
        "from configurations_pkg import *\n",
    )
    .expect("write settings");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("plugin: django: settings `mysite/settings/dev.py:1` star-imports `configurations_pkg`, which was not read; unused files and dependencies are reported below certain"),
        "{stderr}"
    );
}

/// Settings files the plugin does not read are a gap too: one past the files
/// it follows, and one chokkin never discovers that does not parse (#914).
#[test]
fn binary_unread_django_star_imports_are_a_gap() {
    // Past the files the plugin follows, a star import is not read either.
    let project = django_settings_package(
        "import os\n\nos.environ[\"DJANGO_SETTINGS_MODULE\"] = \"mysite.settings.dev\"\n",
    );
    fs::write(
        project.path().join("mysite/settings/dev.py"),
        "from .s1 import *\n",
    )
    .expect("write settings");
    for index in 1..20 {
        fs::write(
            project.path().join(format!("mysite/settings/s{index}.py")),
            format!("from .s{} import *\n", index + 1),
        )
        .expect("write settings");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("plugin: django: settings `mysite/settings/s15.py:1` star-imports `mysite.settings.s16`, which was not read;"),
        "{stderr}"
    );

    // A star-imported file chokkin never discovers is the plugin's to read.
    let project = django_settings_package(
        "import os\n\nos.environ[\"DJANGO_SETTINGS_MODULE\"] = \"mysite.settings.dev\"\n",
    );
    fs::write(
        project.path().join("mysite/settings/dev.py"),
        "from .base import *\nfrom .local import *\n",
    )
    .expect("write settings");
    fs::write(project.path().join(".gitignore"), "local.py\n").expect("write gitignore");
    fs::write(
        project.path().join("mysite/settings/local.py"),
        "DEBUG = (\n",
    )
    .expect("write settings");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("plugin: django: settings `mysite/settings/local.py` could not be read;"),
        "{stderr}"
    );
}

/// [`django_settings_package`] with `manage.py` discovered, so the entry it
/// skips is not what lowers the findings.
fn discovered_manage_django_project() -> tempfile::TempDir {
    let project = django_settings_package(
        "import os\n\nos.environ[\"DJANGO_SETTINGS_MODULE\"] = \"mysite.settings.dev\"\n",
    );
    fs::write(
        project.path().join("pyproject.toml"),
        "[project]\nname = \"mysite\"\nversion = \"0.1.0\"\ndependencies = [\"django\"]\n\n[tool.chokkin]\nproject = [\"manage.py\", \"mysite/**/*.py\"]\n",
    )
    .expect("write pyproject");
    project
}

/// `include()` (django-split-settings), `exec()`, `runpy.run_path()` and
/// `globals().update()` run settings files the plugin does not read or add
/// names it cannot see, so a field the read files do not set may come from
/// them (#925, #937).
#[test]
fn binary_django_settings_include_and_exec_are_a_gap() {
    let level = |code: &str, target: &str, confidence: &str| {
        (
            code.to_owned(),
            target.to_owned(),
            severity(code).to_owned(),
            confidence.to_owned(),
        )
    };
    // Only unused files and distributions depend on what the settings load.
    let levels = |path: &std::path::Path| -> Vec<_> {
        issue_levels(path)
            .into_iter()
            .filter(|(code, ..)| code == "CHK001" || code == "CHK002")
            .collect()
    };
    let cases = [
        (
            "from split_settings.tools import include\n\ninclude(\"components/*.py\")\n",
            "include()",
            3,
        ),
        (
            "import split_settings.tools\n\nif True:\n    split_settings.tools.include(\"local.py\")\n",
            "include()",
            4,
        ),
        (
            "import os\n\nexec(open(os.path.join(os.path.dirname(__file__), \"local.py\")).read())\n",
            "exec()",
            3,
        ),
        (
            "import runpy\n\nglobals().update(runpy.run_path(\"local.py\"))\n",
            "globals().update()",
            3,
        ),
        (
            "from runpy import run_path\n\nLOCAL = run_path(\"local.py\")\n",
            "run_path()",
            3,
        ),
        (
            "import yaml\n\nvars().update(yaml.safe_load(open(\"settings.yaml\")))\n",
            "vars().update()",
            3,
        ),
        (
            "try:\n    locals().update(load())\nexcept ImportError:\n    pass\n",
            "locals().update()",
            2,
        ),
    ];
    for (settings, call, line) in cases {
        let project = discovered_manage_django_project();
        fs::write(project.path().join("mysite/settings/dev.py"), settings).expect("write settings");
        // `ROOT_URLCONF = "mysite.urls"` in `base.py` names the URLconf by
        // its dotted name, so that file falls to info (#1026).
        assert_eq!(
            levels(project.path()),
            [
                level("CHK001", "mysite/settings/base.py", "likely"),
                (
                    "CHK001".to_owned(),
                    "mysite/urls.py".to_owned(),
                    "info".to_owned(),
                    "likely".to_owned(),
                ),
                level("CHK002", "django", "likely"),
            ],
            "{settings}"
        );
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--explain", "CHK002:django"])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        assert!(
            stderr.contains(&format!("plugin: django: settings `mysite/settings/dev.py:{line}` calls `{call}`, which was not read; unused files and dependencies are reported below certain")),
            "{stderr}"
        );
        assert!(
            stderr.contains(&format!("the Django settings could not be found or read: `{call}` in `mysite/settings/dev.py:{line}`")),
            "{stderr}"
        );
    }

    // Settings that set every field they explain need nothing it includes,
    // and a call in a function, class or lambda body does not run when they
    // are imported.
    let project = discovered_manage_django_project();
    fs::write(
        project.path().join("mysite/settings/dev.py"),
        "from split_settings.tools import include\n\nROOT_URLCONF = \"mysite.urls\"\nINSTALLED_APPS = [\"django.contrib.admin\"]\nMIDDLEWARE = []\ninclude(\"local.py\")\n",
    )
    .expect("write settings");
    assert_eq!(
        levels(project.path()),
        [level("CHK001", "mysite/settings/base.py", "certain")]
    );
    fs::write(
        project.path().join("mysite/settings/dev.py"),
        "from .base import *\n\ndef load():\n    exec(\"X = 1\")\n    globals().update({})\n\nclass Local:\n    include(\"local.py\")\n\nrun = lambda: exec(\"X = 1\")\nCACHES = {}\nCACHES.update({})\nvars(Local).update({})\n",
    )
    .expect("write settings");
    fs::write(project.path().join("mysite/orphan.py"), "").expect("write orphan");
    assert_eq!(
        levels(project.path()),
        [level("CHK001", "mysite/orphan.py", "certain")]
    );
}

/// A `manage.py` that names no settings still marks a Django project, whose
/// missing settings are a gap; a reusable app that only depends on Django has
/// no settings of its own, so it gets none (#914).
#[test]
fn binary_django_gap_needs_a_django_project() {
    let stderr = |project: &tempfile::TempDir| {
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache"])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        String::from_utf8(output.stderr).expect("utf8")
    };
    let project = django_settings_package("import django\n");
    for path in ["mysite/settings/base.py", "mysite/settings/dev.py"] {
        fs::remove_file(project.path().join(path)).expect("remove settings");
    }
    let warnings = stderr(&project);
    assert!(
        warnings.contains("plugin: django: `manage.py` sets no DJANGO_SETTINGS_MODULE and no settings.py was found; unused files and dependencies are reported below certain"),
        "{warnings}"
    );

    fs::remove_file(project.path().join("manage.py")).expect("remove manage.py");
    let warnings = stderr(&project);
    assert!(!warnings.contains("plugin: django:"), "{warnings}");
}

/// A `module:symbol` entry whose file discovery left out is skipped like a
/// path entry; one naming no file on disk runs nothing chokkin could read
/// and lowers nothing, as a missing path does (#933).
#[test]
fn binary_module_entry_outside_the_inventory_is_a_skipped_entry() {
    let level = |code: &str, target: &str, confidence: &str| {
        (
            code.to_owned(),
            target.to_owned(),
            severity(code).to_owned(),
            confidence.to_owned(),
        )
    };
    let manifest = |script: &str| {
        format!(
            "[project]\nname = \"acme\"\nversion = \"0\"\ndependencies = [\"requests\"]\n\n[project.scripts]\nacme = \"{script}\"\n\n[tool.chokkin]\nmode = \"app\"\nexclude = [\"acme/cli.py\"]\n"
        )
    };
    let files = |manifest: String| {
        write_project(&[
            ("pyproject.toml", manifest.as_str()),
            ("acme/__init__.py", ""),
            (
                "acme/cli.py",
                "import requests\nfrom acme import helper\n\ndef main():\n    pass\n",
            ),
            ("acme/helper.py", "# helper\n"),
        ])
    };
    let project = files(manifest("acme.cli:main"));
    assert_eq!(
        issue_levels(project.path()),
        [
            level("CHK001", "acme/helper.py", "likely"),
            level("CHK002", "requests", "likely"),
        ]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--no-cache")
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("entry: skipped entry `acme/cli.py`: not a discovered source file; unused files and dependencies are reported below certain"),
        "{stderr}"
    );
    // A production file stays a skipped entry under `--production` too.
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--production"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("skipped entry `acme/cli.py`"), "{stderr}");

    let project = files(manifest("acme.missing:main"));
    assert_eq!(
        issue_levels(project.path()),
        [
            level("CHK001", "acme/helper.py", "certain"),
            level("CHK002", "requests", "certain"),
        ]
    );
}

/// `--production` leaves test files out on purpose, so a target naming one
/// is dropped like any non-production entry rather than skipped (#933).
#[test]
fn binary_production_module_entry_in_tests_is_not_a_skipped_entry() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0\"\ndependencies = [\"requests\"]\n\n[project.scripts]\nacme = \"tests.app:main\"\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/helper.py", "# helper\n"),
        ("tests/__init__.py", ""),
        ("tests/app.py", "import requests\n\ndef main():\n    pass\n"),
    ]);
    let levels: Vec<(String, String)> =
        json_issues(project.path(), &["--no-cache", "--production"])
            .iter()
            .map(|issue| {
                let field = |key: &str| issue[key].as_str().unwrap_or_default().to_owned();
                (field("target"), field("confidence"))
            })
            .collect();
    assert!(
        levels.contains(&("requests".to_owned(), "certain".to_owned())),
        "{levels:?}"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--production"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(!stderr.contains("skipped entry"), "{stderr}");
}

/// A member's build hook outside the member's own tree is not an entry the
/// root skipped: it is dropped as the root drops one, lowering nothing (#934).
#[test]
fn binary_member_build_script_outside_its_tree_is_not_a_skipped_entry() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"root\"\nversion = \"0\"\ndependencies = [\"requests\"]\n\n[project.scripts]\nroot = \"root_pkg.cli:main\"\n\n[tool.uv.workspace]\nmembers = [\"pkgs/a\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("root_pkg/__init__.py", ""),
        ("root_pkg/cli.py", "def main():\n    pass\n"),
        (
            "pkgs/a/pyproject.toml",
            "[project]\nname = \"a\"\nversion = \"0\"\n\n[tool.hatch.build.hooks.custom]\npath = \"../../tools/hook.py\"\n",
        ),
        ("pkgs/a/a/__init__.py", ""),
        ("tools/hook.py", "# hook\n"),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--no-cache")
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(!stderr.contains("skipped entry"), "{stderr}");
    let levels: Vec<(String, String)> = json_issues(project.path(), &["--no-cache"])
        .iter()
        .map(|issue| {
            let field = |key: &str| issue[key].as_str().unwrap_or_default().to_owned();
            (field("target"), field("confidence"))
        })
        .collect();
    assert!(
        levels.contains(&("requests".to_owned(), "certain".to_owned())),
        "{levels:?}"
    );
}

/// A member's manifest entry the root excludes is skipped like a root one:
/// its imports are unknown, so what it may use falls to likely (#934).
#[test]
fn binary_member_entry_outside_the_root_inventory_is_a_skipped_entry() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"root\"\nversion = \"0\"\n\n[tool.uv.workspace]\nmembers = [\"packages/tool\"]\n\n[tool.chokkin]\nexclude = [\"packages/tool/src/tool/cli.py\"]\n",
        ),
        (
            "packages/tool/pyproject.toml",
            "[project]\nname = \"tool\"\nversion = \"0\"\n\n[project.scripts]\ntool = \"tool.cli:main\"\n",
        ),
        ("packages/tool/src/tool/__init__.py", ""),
        (
            "packages/tool/src/tool/cli.py",
            "from tool import helper\n\ndef main():\n    pass\n",
        ),
        ("packages/tool/src/tool/helper.py", "# helper\n"),
    ]);
    assert_eq!(
        issue_levels(project.path()),
        [(
            "CHK001".to_owned(),
            "packages/tool/src/tool/helper.py".to_owned(),
            "warning".to_owned(),
            "likely".to_owned(),
        )]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--no-cache")
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains(
            "entry: skipped entry `packages/tool/src/tool/cli.py`: not a discovered source file"
        ),
        "{stderr}"
    );
}

/// An entry chokkin skips because it is not a discovered source runs code
/// whose imports are unknown, so the files and distributions it may use fall
/// to likely; one that does not exist hides nothing (#923).
#[test]
fn binary_skipped_entry_lowers_unused_files_and_distributions() {
    let level = |code: &str, target: &str, confidence: &str| {
        (
            code.to_owned(),
            target.to_owned(),
            severity(code).to_owned(),
            confidence.to_owned(),
        )
    };
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"mysite\"\nversion = \"0.1.0\"\ndependencies = [\"django\"]\n\n[tool.chokkin]\nexclude = [\"manage.py\"]\n",
        ),
        (
            "manage.py",
            "import os\n\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"mysite.settings\")\nfrom mysite import helper\n",
        ),
        ("mysite/__init__.py", ""),
        // No public name: Django reading settings is not what this checks.
        ("mysite/settings.py", "# settings\n"),
        ("mysite/helper.py", "# helper\n"),
    ]);
    assert_eq!(
        issue_levels(project.path()),
        [
            level("CHK001", "mysite/helper.py", "likely"),
            level("CHK002", "django", "likely"),
        ]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--explain", "CHK002:django"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("entry: skipped entry `manage.py`: not a discovered source file; unused files and dependencies are reported below certain"),
        "{stderr}"
    );
    assert!(
        stderr.contains("an entry point is not a discovered source file: `manage.py`"),
        "{stderr}"
    );

    // A config entry that does not exist runs nothing.
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"acme/missing.py\"]\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/helper.py", "# helper\n"),
    ]);
    assert_eq!(
        issue_levels(project.path()),
        [
            level("CHK001", "acme/helper.py", "certain"),
            level("CHK002", "requests", "certain"),
        ]
    );
}

/// A file a plugin makes an entry is read even where the layout's globs do
/// not reach: its imports count, and no skipped entry lowers the unrelated
/// findings (#932).
#[test]
fn binary_plugin_entries_outside_the_globs_are_read() {
    // (pyproject, files, the orphan that stays reported)
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
    let level = |code: &str, target: &str| {
        (
            code.to_owned(),
            target.to_owned(),
            severity(code).to_owned(),
            "certain".to_owned(),
        )
    };
    let cases: [Case; 3] = [
        // A flat layout scans its package, not the root's scripts.
        (
            "[project]\nname = \"mysite\"\nversion = \"0.1.0\"\ndependencies = [\"django\", \"requests\", \"httpx\"]\n",
            &[
                (
                    "manage.py",
                    "import os\nimport requests\nfrom django.core.management import execute_from_command_line\n\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"mysite.settings\")\nfrom mysite import helper\n",
                ),
                ("mysite/__init__.py", ""),
                ("mysite/settings.py", "# settings\n"),
                ("mysite/helper.py", "# helper\n"),
            ],
            "mysite/orphan.py",
        ),
        // Explicit `project` globs leave out the docs.
        (
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"httpx\"]\n\n[project.optional-dependencies]\ndocs = [\"sphinx\", \"furo\"]\n\n[tool.chokkin]\nmode = \"app\"\nproject = [\"src/**/*.py\"]\nentry = [\"src/acme/__init__.py\"]\n",
            &[
                ("docs/conf.py", "import furo\nfrom acme import helper\n"),
                ("src/acme/__init__.py", ""),
                ("src/acme/helper.py", "# helper\n"),
            ],
            "src/acme/orphan.py",
        ),
        // Alembic's default environment sits beside a flat layout's package.
        (
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"alembic\", \"sqlalchemy\", \"httpx\"]\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"acme/__init__.py\"]\n",
            &[
                (
                    "alembic/env.py",
                    "from alembic import context\nimport sqlalchemy\nfrom acme import helper\n",
                ),
                ("acme/__init__.py", ""),
                ("acme/helper.py", "# helper\n"),
            ],
            "acme/orphan.py",
        ),
    ];
    for (pyproject, files, orphan) in cases {
        let mut files = files.to_vec();
        files.extend([("pyproject.toml", pyproject), (orphan, "# orphan\n")]);
        let project = write_project(&files);
        assert_eq!(
            issue_levels(project.path()),
            [level("CHK001", orphan), level("CHK002", "httpx")],
            "{pyproject}"
        );
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .arg("--no-cache")
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        assert!(!stderr.contains("skipped entry"), "{stderr}");
        // The added file is in the parse cache key, cold and warm.
        let uncached = json_issues(project.path(), &["--no-cache"]);
        for run in ["cold", "warm"] {
            assert_eq!(json_issues(project.path(), &[]), uncached, "{run}");
        }
    }
}

/// A plugin entry the globs left out and the parser cannot read is a parse
/// gap, not a skipped entry: its imports are unknown, so unrelated unused
/// findings stay below certain (#932, ADR 0005).
#[test]
fn binary_unreadable_plugin_entry_outside_the_globs_is_a_parse_gap() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"httpx\"]\n\n[project.optional-dependencies]\ndocs = [\"sphinx\"]\n\n[tool.chokkin]\nmode = \"app\"\nproject = [\"src/**/*.py\"]\nentry = [\"src/acme/__init__.py\"]\n",
        ),
        ("docs/conf.py", "def broken(:\n"),
        ("src/acme/__init__.py", ""),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--no-cache")
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("parse: skipped `docs/conf.py`: syntax error"),
        "{stderr}"
    );
    assert!(!stderr.contains("skipped entry"), "{stderr}");
    let levels = issue_levels(project.path());
    assert!(
        levels
            .iter()
            .any(|(code, target, _, confidence)| code == "CHK002"
                && target == "httpx"
                && confidence != "certain"),
        "{levels:?}"
    );
}

/// An unreadable `requires-python` leaves py311 to decide which modules are
/// stdlib, so an import of one that only other versions ship becomes a hint;
/// other imports keep their level, and an explicit `target_version` assumes
/// nothing (#924).
#[test]
fn binary_unreadable_requires_python_turns_version_dependent_imports_into_hints() {
    let levels = |root: &std::path::Path| -> Vec<(String, String, String, String)> {
        json_issues(root, &["--no-cache", "--confidence", "maybe"])
            .iter()
            .map(|issue| {
                let field = |key: &str| issue[key].as_str().unwrap_or_default().to_owned();
                (
                    field("code"),
                    field("target"),
                    field("severity"),
                    field("confidence"),
                )
            })
            .collect()
    };
    let level = |code: &str, target: &str, severity: &str, confidence: &str| {
        (
            code.to_owned(),
            target.to_owned(),
            severity.to_owned(),
            confidence.to_owned(),
        )
    };
    let pyproject = "[project]\nname = \"acme\"\nversion = \"0.1.0\"\nrequires-python = \"3.10+\"\ndependencies = []\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"acme/main.py\"]\n";
    let files = |pyproject: &'static str| {
        write_project(&[
            ("pyproject.toml", pyproject),
            ("acme/__init__.py", ""),
            // `binhex` is stdlib in 3.10 only; `yaml` in none.
            ("acme/main.py", "import binhex\nimport yaml\n"),
            // A script with its own target assumes nothing.
            (
                "acme/tool.py",
                "# /// script\n# requires-python = \">=3.11\"\n# dependencies = []\n# ///\nimport binhex\n",
            ),
        ])
    };
    let project = files(pyproject);
    assert_eq!(
        levels(project.path()),
        [
            level("CHK003", "acme/main.py:yaml", "error", "certain"),
            level("CHK010", "acme/main.py:binhex", "info", "maybe"),
            level("CHK010", "acme/tool.py:binhex", "warning", "likely"),
        ]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args([
            "--no-cache",
            "--confidence",
            "maybe",
            "--explain",
            "CHK010:binhex",
        ])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("manifest: could not read `requires-python` \"3.10+\" in `pyproject.toml`; assuming target version py311; imports of modules only other versions ship as stdlib are reported as hints"),
        "{stderr}"
    );
    assert!(
        stderr.contains("`requires-python` could not be read, so py311 decided the module is not stdlib: `binhex` in `acme/main.py:1`"),
        "{stderr}"
    );

    let project = files(
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\nrequires-python = \"3.10+\"\ndependencies = []\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"acme/main.py\"]\ntarget_version = \"py311\"\n",
    );
    assert_eq!(
        levels(project.path()),
        [
            level("CHK003", "acme/main.py:yaml", "error", "certain"),
            level("CHK010", "acme/main.py:binhex", "warning", "likely"),
            level("CHK010", "acme/tool.py:binhex", "warning", "likely"),
        ]
    );
}

/// An invalid requirement line may have declared the distribution its
/// leading name names, so an import of that one becomes a hint; a line with
/// no name lowers nothing, and other imports keep their level (#928).
#[test]
fn binary_invalid_requirement_line_turns_imports_of_its_name_into_hints() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"acme/main.py\"]\n",
        ),
        // `rich_` cannot end a PEP 508 name, so the name read is `rich`.
        // pip rejects an option it does not know, so the click line is
        // invalid even though its requirement part parses (#927).
        (
            "requirements.txt",
            "PyYAML>=\nrich_ >=13\n>=1.0\nclick==8.1 --unknown\n",
        ),
        ("acme/__init__.py", ""),
        (
            "acme/main.py",
            "import yaml\nimport rich\nimport click\nimport idna\n",
        ),
    ]);
    let levels: Vec<_> = json_issues(project.path(), &["--no-cache", "--confidence", "maybe"])
        .iter()
        .map(|issue| {
            let field = |key: &str| issue[key].as_str().unwrap_or_default().to_owned();
            format!(
                "{} {} {} {}",
                field("code"),
                field("target"),
                field("severity"),
                field("confidence"),
            )
        })
        .collect();
    assert_eq!(
        levels,
        [
            "CHK003 acme/main.py:yaml info maybe",
            "CHK003 acme/main.py:rich info maybe",
            "CHK003 acme/main.py:click info maybe",
            "CHK003 acme/main.py:idna error certain",
        ]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args([
            "--no-cache",
            "--confidence",
            "maybe",
            "--explain",
            "CHK003:yaml",
        ])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    for warning in [
        "manifest: invalid requirement at `requirements.txt:1`: PyYAML>=; undeclared or misplaced imports of `pyyaml` are reported as hints",
        "manifest: invalid requirement at `requirements.txt:2`: rich_ >=13; undeclared or misplaced imports of `rich` are reported as hints",
        "manifest: invalid requirement at `requirements.txt:3`: >=1.0\n",
    ] {
        assert!(stderr.contains(warning), "{warning}\n{stderr}");
    }
    assert!(
        stderr.contains("a requirement line naming the distribution could not be read: `pyyaml` in `requirements.txt:1`"),
        "{stderr}"
    );
}

/// An archive line pip installs from a file name that names its
/// distribution is not read, so imports of that name become hints, resolved
/// or not, and a runtime one may be the runtime declaration of a dev-only
/// distribution; a directory line names nothing and lowers nothing (#988).
#[test]
fn binary_archive_requirement_turns_imports_of_its_name_into_hints() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0\"\ndependencies = []\n\n[dependency-groups]\ndev = [\"qux\"]\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"acme/main.py\"]\n",
        ),
        (
            "requirements.txt",
            "./vendor/foo\nwheels/bar-1.0-py3-none-any.whl\nhttps://example.com/baz-2.0.tar.gz#sha256=0\n./dist/qux-1.0.zip\nwheels/rich-13.0-py3-none-any.whl\n-e wheels/quux-1.0.tar.gz\n",
        ),
        ("acme/__init__.py", ""),
        (
            "acme/main.py",
            "import foo\nimport bar\nimport baz.core\nimport qux\nimport rich\nimport idna\nimport quux\n",
        ),
    ]);
    let levels: Vec<_> = json_issues(project.path(), &["--no-cache", "--confidence", "maybe"])
        .iter()
        .map(|issue| {
            let field = |key: &str| issue[key].as_str().unwrap_or_default().to_owned();
            format!(
                "{} {} {} {}",
                field("code"),
                field("target"),
                field("severity"),
                field("confidence"),
            )
        })
        .collect();
    assert_eq!(
        levels,
        [
            "CHK003 acme/main.py:rich info maybe",
            "CHK003 acme/main.py:idna error certain",
            "CHK005 qux info maybe",
            "CHK010 acme/main.py:foo warning likely",
            "CHK010 acme/main.py:bar info maybe",
            "CHK010 acme/main.py:baz info maybe",
            // An editable line names a directory, never an archive.
            "CHK010 acme/main.py:quux warning likely",
        ]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args([
            "--no-cache",
            "--confidence",
            "maybe",
            "--explain",
            "CHK003:rich",
        ])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("manifest: `requirements.txt:2` installs `bar` and 3 more distributions from archives chokkin does not read; findings on imports of them are reported as hints"),
        "{stderr}"
    );
    assert!(
        stderr.contains("a requirement line installs an archive named for the distribution, which is not read: `rich` in `requirements.txt:5`"),
        "{stderr}"
    );
}

/// An unreadable runtime line may be the runtime declaration of a dev-only
/// distribution, so its runtime import is a misplaced hint; a valid dev
/// line beside it does not settle where the distribution belongs (#935).
#[test]
fn binary_invalid_runtime_requirement_turns_misplaced_findings_into_hints() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0\"\ndependencies = [\"rich>=\"]\n\n[dependency-groups]\ndev = [\"rich\", \"pytest\", \"black>=\"]\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/main.py", "import rich\nimport pytest\n"),
    ]);
    let levels: Vec<_> = json_issues(project.path(), &["--no-cache", "--confidence", "maybe"])
        .iter()
        .map(|issue| {
            let field = |key: &str| issue[key].as_str().unwrap_or_default().to_owned();
            format!(
                "{} {} {} {}",
                field("code"),
                field("target"),
                field("severity"),
                field("confidence"),
            )
        })
        .collect();
    assert_eq!(
        levels,
        ["CHK005 pytest warning certain", "CHK005 rich info maybe"]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--confidence", "maybe"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    for warning in [
        "(project.dependencies[0]): rich>=; undeclared or misplaced imports of `rich` are reported as hints",
        "(dependency-groups.dev[2]): black>=; undeclared imports of `black` are reported as hints",
    ] {
        assert!(stderr.contains(warning), "{warning}\n{stderr}");
    }
}

/// #936: what a PEP 723 block could not read lowers that script's findings
/// alone. An unreadable `requires-python` makes its version-dependent imports
/// hints, and an invalid `dependencies` line its imports of the name it names;
/// the other script, read in full, keeps its findings.
#[test]
fn binary_unread_script_block_turns_that_scripts_findings_into_hints() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0\"\nrequires-python = \">=3.11\"\ndependencies = []\n",
        ),
        (
            "tools/a.py",
            "# /// script\n# requires-python = \"bogus\"\n# dependencies = [\"rich>=\"]\n# ///\nimport binhex\nimport rich\nimport httpx\n",
        ),
        (
            "tools/b.py",
            "# /// script\n# dependencies = []\n# ///\nimport binhex\nimport rich\n",
        ),
        // A module that is not the script reads no imports for it.
        ("tools/__init__.py", ""),
    ]);
    let levels: Vec<_> = json_issues(project.path(), &["--no-cache", "--confidence", "maybe"])
        .iter()
        .map(|issue| {
            let field = |key: &str| issue[key].as_str().unwrap_or_default().to_owned();
            format!(
                "{} {} {} {}",
                field("code"),
                field("target"),
                field("severity"),
                field("confidence"),
            )
        })
        .collect();
    assert_eq!(
        levels,
        [
            "CHK003 script:tools/a.py:httpx error certain",
            "CHK003 script:tools/a.py:rich info maybe",
            "CHK003 script:tools/b.py:rich error certain",
            "CHK010 tools/a.py:binhex info maybe",
            "CHK010 tools/b.py:binhex warning likely",
        ]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--confidence", "maybe"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    for warning in [
        "at `tools/a.py:3`: rich>=; undeclared imports of `rich` are reported as hints",
        "of `tools/a.py`; using the project's target version; its imports of modules only other versions ship as stdlib are reported as hints",
    ] {
        assert!(stderr.contains(warning), "{warning}\n{stderr}");
    }
}

/// What the parser could not read in a file it kept reaches stderr and the
/// JSON diagnostics, so a finding it causes has a visible reason (#877).
#[test]
fn binary_parse_warnings_reach_stderr_and_json_diagnostics() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        (
            "acme/__init__.py",
            "from acme import other\n\n__all__ = [\"VALUE\"]\n__all__ += other.__all__\nVALUE = 1\n",
        ),
        ("acme/other.py", "__all__ = [\"OTHER\"]\nOTHER = 2\n"),
        ("scripts/tool.py", "from . import helper\n"),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    let expected = [
        "parse: `acme/__init__.py:4`: unsupported `__all__` assignment form",
        "parse: `scripts/tool.py:1`: could not resolve relative import (file is not inside a known package)",
    ];
    for line in expected {
        assert!(stderr.lines().any(|seen| seen == line), "{line}\n{stderr}");
    }
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let diagnostics: Vec<_> = parsed["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter_map(|diagnostic| diagnostic["message"].as_str())
        .collect();
    for line in expected {
        assert!(diagnostics.contains(&line), "{line}\n{diagnostics:?}");
    }
}

/// A package under a directory the layout does not name (django's
/// `tests/<app>/`, which `tests/runtests.py` imports top-level) takes its
/// package context from the `__init__.py` chain, so its relative imports
/// resolve to the sibling files instead of warning (#896). Such an import
/// stays first-party even when its root shares a dependency's name.
#[test]
fn binary_relative_imports_resolve_in_packages_outside_the_layout() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", "VALUE = 1\n"),
        ("tests/app/__init__.py", ""),
        ("tests/app/models.py", "class Thing:\n    pass\n"),
        ("tests/app/views.py", "def view():\n    pass\n"),
        (
            "tests/app/test_admin.py",
            "from .models import Thing\nfrom . import views\n\n\ndef test_thing():\n    assert Thing and views\n",
        ),
        ("scripts/pkg/__init__.py", ""),
        ("scripts/pkg/main.py", "from .helpers import run\n\nrun()\n"),
        ("scripts/pkg/helpers.py", "def run():\n    pass\n"),
        ("scripts/requests/__init__.py", ""),
        (
            "scripts/requests/main.py",
            "from .api import get\n\nget()\n",
        ),
        ("scripts/requests/api.py", "def get():\n    pass\n"),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(!stderr.contains("relative import"), "{stderr}");
    assert!(!stderr.contains("unresolved import"), "{stderr}");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let unused: Vec<_> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .filter(|issue| matches!(issue["code"].as_str(), Some("CHK001" | "CHK002")))
        .filter_map(|issue| issue["target"].as_str())
        .collect();
    assert_eq!(unused, ["requests"]);
}

/// An entry point that maps to no source file reaches stderr, for the root
/// manifest and for a workspace member's own manifest alike. A config entry
/// missing on disk is reported once, by source discovery.
#[test]
fn binary_entry_warnings_reach_stderr() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [project.scripts]\nacme = \"acme.cli:main\"\n\n\
             [tool.uv.workspace]\nmembers = [\"packages/tool\"]\n\n\
             [tool.chokkin]\nmode = \"app\"\nexclude = [\"acme/excluded.py\"]\n\
             entry = [\"acme/excluded.py\", \"acme/gone.py\"]\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/excluded.py", ""),
        (
            "packages/tool/pyproject.toml",
            "[project]\nname = \"tool\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [project.scripts]\ntool = \"tool.missing:run\"\n",
        ),
        ("packages/tool/tool/__init__.py", ""),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    for line in [
        "entry: could not map `acme.cli` to a source file (`project.scripts.acme` in `pyproject.toml`)",
        "entry: could not map `tool.missing` to a source file (`project.scripts.tool` in `packages/tool/pyproject.toml`)",
        "entry: skipped entry `acme/excluded.py`: not a discovered source file; unused files and dependencies are reported below certain",
    ] {
        assert!(stderr.lines().any(|seen| seen == line), "{line}\n{stderr}");
    }
    assert_eq!(
        stderr.matches("acme/gone.py").count(),
        1,
        "reported by sources only\n{stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let diagnostics = parsed["diagnostics"].to_string();
    assert!(
        diagnostics.contains("could not map `acme.cli`"),
        "{diagnostics}"
    );
}

/// Member manifests and the extra requirements files are read like the root
/// manifest, so what they could not read is reported too, under root-relative
/// paths (#888).
#[test]
fn binary_member_and_extra_requirements_warnings_reach_stderr() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [tool.uv.workspace]\nmembers = [\"packages/*\"]\n",
        ),
        ("acme/__init__.py", ""),
        ("requirements/docs.txt", "sphinx\nnot a requirement!!\n"),
        ("requirements/ci.txt", "pytest\n-r missing-base.txt\n"),
        (
            "packages/tool/pyproject.toml",
            "[project]\nname = \"tool\"\nversion = \"0.1.0\"\n\
             dependencies = [\"requests >= 2 ;;\"]\n\n[tool.poetry]\n",
        ),
        ("packages/tool/tool/__init__.py", ""),
        (
            "packages/dyn/pyproject.toml",
            "[project]\nname = \"dyn\"\nversion = \"0.1.0\"\ndynamic = [\"dependencies\"]\n",
        ),
        ("packages/dyn/dyn/__init__.py", ""),
        (
            "packages/old/pyproject.toml",
            "[project]\nname = \"old\"\nversion = \"0.1.0\"\nrequires-python = \"3.10+\"\n",
        ),
        ("packages/old/old/__init__.py", ""),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    for line in [
        "manifest: invalid requirement at `requirements/docs.txt:2`: not a requirement!!",
        // Only the root's `requires-python` picks the stdlib, so a member's
        // lowers nothing and the warning claims no hints.
        "manifest: could not read `requires-python` \"3.10+\" in `packages/old/pyproject.toml`; assuming target version py311",
        "manifest: stopped reading `requirements/ci.txt`: requirements file not found: missing-base.txt",
        "manifest: invalid requirement at `packages/tool/pyproject.toml` (project.dependencies[0]): requests >= 2 ;;; undeclared or misplaced imports of `requests` are reported as hints",
        "manifest: `packages/tool`: Poetry sections detected (partial dependency extraction)",
        "manifest: runtime dependencies in `packages/dyn/pyproject.toml` could not be read statically; missing-dependency findings in `packages/dyn` are reported as low-confidence info",
    ] {
        assert!(stderr.lines().any(|seen| seen == line), "{line}\n{stderr}");
    }
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let diagnostics = parsed["diagnostics"].to_string();
    assert!(
        diagnostics.contains("packages/dyn/pyproject.toml"),
        "{diagnostics}"
    );
}

/// A member whose runtime dependencies could not be read may declare what
/// its own code imports, so only its missing declarations fall to hints
/// (shown under `--confidence maybe`); a member whose declarations were read
/// keeps its findings certain (#910).
#[test]
fn binary_member_unread_dependencies_lower_only_its_own_findings() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"root\"\nversion = \"0.1.0\"\n\n\
             [tool.uv.workspace]\nmembers = [\"packages/*\"]\n",
        ),
        (
            "packages/x/pyproject.toml",
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\ndynamic = [\"dependencies\"]\n\n\
             [project.scripts]\nx = \"x.main:main\"\n",
        ),
        ("packages/x/x/__init__.py", ""),
        (
            "packages/x/x/main.py",
            "import requests\n\ndef main():\n    pass\n",
        ),
        (
            "packages/y/pyproject.toml",
            "[project]\nname = \"y\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [project.scripts]\ny = \"y.main:main\"\n",
        ),
        ("packages/y/y/__init__.py", ""),
        (
            "packages/y/y/main.py",
            "import requests\n\ndef main():\n    pass\n",
        ),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json", "--confidence", "maybe"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let mut issues: Vec<_> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|issue| {
            (
                issue["code"].as_str().unwrap_or_default().to_owned(),
                issue["path"].as_str().unwrap_or_default().to_owned(),
                issue["severity"].as_str().unwrap_or_default().to_owned(),
                issue["confidence"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    issues.sort();
    let expected = [
        ("CHK003", "packages/x/x/main.py", "info", "maybe"),
        ("CHK003", "packages/y/y/main.py", "error", "certain"),
    ]
    .map(|(code, path, severity, confidence)| {
        (
            code.to_owned(),
            path.to_owned(),
            severity.to_owned(),
            confidence.to_owned(),
        )
    });
    assert_eq!(issues, expected, "{parsed}");
}

/// A member's source discovery is reported like the root's, under
/// root-relative paths (#892).
#[test]
fn binary_member_sources_warnings_reach_every_reporter() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [tool.uv.workspace]\nmembers = [\"packages/*\"]\n",
        ),
        ("acme/__init__.py", ""),
        (
            "packages/tool/pyproject.toml",
            "[project]\nname = \"tool\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [tool.chokkin]\nentry = [\"missing.py\"]\n",
        ),
        ("packages/tool/other/__init__.py", ""),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    let lines = [
        "sources: missing entry path `packages/tool/missing.py`",
        "sources: no package directory matches project `tool`; guessed `packages/tool/other` \
         (declare it in the build backend config or [tool.chokkin] project)",
    ];
    for line in lines {
        assert!(stderr.lines().any(|seen| seen == line), "{line}\n{stderr}");
    }
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let diagnostics = parsed["diagnostics"].to_string();
    for line in lines {
        assert!(diagnostics.contains(line), "{line}\n{diagnostics}");
    }
}

/// An extra requirements file that stops at a missing include still watches
/// that include, so creating it refreshes the cached run (#888).
#[test]
fn binary_cached_run_sees_a_created_extra_requirements_include() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        ("acme/__init__.py", ""),
        ("requirements/ci.txt", "-r ../shared.txt\n"),
    ]);
    let run = || {
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--reporter", "json"])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        String::from_utf8(output.stderr).expect("utf8")
    };
    let stopped = "manifest: stopped reading `requirements/ci.txt`";

    let stderr = run();
    assert!(stderr.contains(stopped), "{stderr}");

    fs::write(project.path().join("shared.txt"), "pytest\n").expect("write include");
    let stderr = run();
    assert!(!stderr.contains(stopped), "{stderr}");
}

/// An include shared by several requirements files is read once per includer,
/// but its warning is printed once (#888).
#[test]
fn binary_prints_a_shared_include_warning_once() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        ("acme/__init__.py", ""),
        (
            "requirements/base.txt",
            "--index-url https://pypi.example/simple\nrequests\n",
        ),
        ("requirements/dev.txt", "-r base.txt\npytest\n"),
        ("requirements/test.txt", "-r base.txt\ncoverage\n"),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    let line = "manifest: ignored requirements option at `requirements/base.txt:1`: --index-url https://pypi.example/simple";
    assert_eq!(
        stderr.lines().filter(|seen| *seen == line).count(),
        1,
        "{stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let diagnostics = parsed["diagnostics"].as_array().expect("diagnostics");
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.to_string().contains("requirements/base.txt:1"))
            .count(),
        1,
        "{diagnostics:?}"
    );
}

/// stderr cuts a repeated warning kind to five lines and a count, while JSON
/// keeps every warning (#888).
#[test]
fn binary_condenses_repeated_warnings_on_stderr_only() {
    let mut files = vec![
        (
            "pyproject.toml".to_owned(),
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n".to_owned(),
        ),
        ("acme/__init__.py".to_owned(), String::new()),
    ];
    for n in 1..=7 {
        files.push((
            format!("requirements/r{n}.txt"),
            "not a requirement!!\n".to_owned(),
        ));
    }
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, contents)| (path.as_str(), contents.as_str()))
        .collect();
    let project = write_project(&files);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("manifest: invalid requirement at"))
            .count(),
        5,
        "{stderr}"
    );
    assert!(
        stderr.lines().any(|line| line
            == "manifest: … and 2 more like the above (`--reporter json` lists every warning)"),
        "{stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let invalid = parsed["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|diagnostic| diagnostic.to_string().contains("invalid requirement"))
        .count();
    assert_eq!(invalid, 7);
}

/// The resolver's ambiguous mappings and an unreadable `.venv` lower or drop
/// what resolution knows, so they are reported too (#888).
#[test]
fn binary_resolver_warnings_reach_stderr() {
    let ambiguous = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        ("acme/__init__.py", "import shared\n"),
        (
            ".venv/lib/python3.11/site-packages/alpha-1.0.dist-info/METADATA",
            "Name: alpha\n",
        ),
        (
            ".venv/lib/python3.11/site-packages/alpha-1.0.dist-info/top_level.txt",
            "shared\n",
        ),
        (
            ".venv/lib/python3.11/site-packages/beta-1.0.dist-info/METADATA",
            "Name: beta\n",
        ),
        (
            ".venv/lib/python3.11/site-packages/beta-1.0.dist-info/top_level.txt",
            "shared\n",
        ),
    ]);
    let unreadable = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        ("acme/__init__.py", ""),
        (".venv/pyvenv.cfg", ""),
    ]);

    for (project, line) in [
        (
            &ambiguous,
            "resolve: import `shared` maps to several distributions (alpha, beta)",
        ),
        (
            &unreadable,
            "resolve: could not read virtualenv `.venv`: site-packages not found",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--reporter", "json"])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        assert!(stderr.lines().any(|seen| seen == line), "{line}\n{stderr}");
        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
        assert!(
            parsed["diagnostics"].to_string().contains("resolve: "),
            "{}",
            parsed["diagnostics"]
        );
    }
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
    // The only finding is an unused file, a warning (spec §3).
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::Success.code().into())
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

/// fastmcp: a root whose console script runs a member's package and a member
/// with its own CLI resolve to app, yet each wheel ships its package's
/// `__all__` as public API, so those re-exports are not CHK007. A name outside
/// `__all__`, and an `__all__` its own member's wheel does not ship, stay
/// reported (#678).
#[test]
fn binary_wheel_all_reexports_are_not_chk007_in_app_mode() {
    let hatch = "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n";
    let root = format!(
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[project.scripts]\nacme = \"acme.cli:main\"\nacme-serve = \"acme_cli.main:main\"\n\n{hatch}\n[tool.hatch.build.targets.wheel]\npackages = [\"src/acme\", \"src/demo-app\"]\n\n[tool.uv.workspace]\nmembers = [\"src/acme-cli\", \"src/demo-app\"]\n"
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

/// A root console script into a member's package makes the root an app; a
/// shipped module's `__all__` is still its public API even when only the
/// module itself uses a name (#727, `fastmcp_slim`).
#[test]
fn shipped_all_names_are_not_chk006_in_app_mode() {
    let hatch = "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n";
    let root = format!(
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[project.scripts]\nacme = \"acme.cli:main\"\nacme-one = \"one:main\"\n\n{hatch}\n[tool.hatch.build.targets.wheel]\npackages = [\"src/acme\"]\n\n[tool.uv.workspace]\nmembers = [\"plugins/one\", \"plugins/two\"]\n"
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
        ("plugins/one/one/__init__.py", "def main():\n    pass\n"),
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

/// mlflow: a workspace root that ships its own package is a library however
/// many members it has, so its public modules are neither CHK001 nor CHK006
/// warnings (#776).
#[test]
fn workspace_root_shipping_its_package_is_a_library() {
    let hatch = "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n";
    let root = format!(
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n{hatch}\n[tool.hatch.build.targets.wheel]\npackages = [\"src/acme\"]\n\n[tool.uv.workspace]\nmembers = [\"plugins/one\", \"plugins/two\"]\n"
    );
    let member = |name: &str| {
        format!(
            "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n{hatch}\n[tool.hatch.build.targets.wheel]\npackages = [\"{name}\"]\n"
        )
    };
    let (one, two) = (member("one"), member("two"));
    let project = write_project(&[
        ("pyproject.toml", root.as_str()),
        ("src/acme/__init__.py", ""),
        (
            "src/acme/artifacts.py",
            "def load_image(path):\n    return path\n",
        ),
        ("plugins/one/pyproject.toml", one.as_str()),
        ("plugins/one/one/__init__.py", ""),
        ("plugins/two/pyproject.toml", two.as_str()),
        ("plugins/two/two/__init__.py", ""),
    ]);
    let warnings: Vec<_> = json_issues(project.path(), &[])
        .into_iter()
        .filter(|issue| issue["severity"] != "info")
        .collect();
    assert!(warnings.is_empty(), "{warnings:?}");
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

/// Sphinx loads `conf.py`'s extensions and theme, and pytest the `pytest-*`
/// plugins, without an import (#809).
#[test]
fn sphinx_extensions_themes_and_pytest_plugins_are_used() {
    let temp = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"pkg\"\nversion = \"0\"\n\n[dependency-groups]\n\
             docs = [\"sphinx\", \"furo\", \"sphinxcontrib-log-cabinet\", \
             \"sphinxext-opengraph\", \"sphinx-issues\"]\n\
             test = [\"pytest\", \"pytest-xdist\"]\n",
        ),
        (
            "docs/conf.py",
            "extensions = [\"sphinxcontrib.log_cabinet\"]\n\
             extensions.append(\"sphinxext.opengraph\")\nhtml_theme = \"furo\"\n",
        ),
        ("pkg/__init__.py", ""),
        (
            "tests/test_pkg.py",
            "import pytest\n\ndef test_pkg():\n    assert pytest\n",
        ),
    ]);

    let keys = issue_keys(&json_issues(temp.path(), &["--no-cache", "--strict"]));
    let unused: Vec<&str> = keys
        .iter()
        .filter(|(code, _)| code == "CHK002")
        .map(|(_, target)| target.as_str())
        .collect();
    assert_eq!(unused, ["sphinx-issues"], "{keys:?}");
}

/// airflow's `sphinx-argparse` and `sphinx-autoapi` extensions load as
/// `sphinxarg` and `autoapi`, which name matching cannot tie to the
/// distribution (#822).
#[test]
fn sphinx_extensions_named_unlike_their_distribution_resolve() {
    let temp = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"pkg\"\nversion = \"0\"\n\n[project.optional-dependencies]\n\
             docs = [\"sphinx\", \"sphinx-argparse\", \"sphinx-autoapi\", \
             \"sphinx-notfound-page\", \"sphinx-autodoc2\"]\n",
        ),
        (
            "docs/conf.py",
            "extensions = [\"sphinxarg.ext\", \"autoapi.extension\", \
             \"notfound.extension\", \"autodoc2\"]\n",
        ),
        ("pkg/__init__.py", ""),
    ]);

    let keys = issue_keys(&json_issues(temp.path(), &["--no-cache", "--strict"]));
    assert!(
        keys.iter()
            .all(|(code, _)| !matches!(code.as_str(), "CHK002" | "CHK010")),
        "{keys:?}"
    );
}

/// black's hatch-vcs `version-file` and sqlalchemy's `cython` build
/// requirement exist only at build time (#720).
#[test]
fn build_time_modules_are_not_chk010() {
    let temp = write_project(&[
        (
            "pyproject.toml",
            "[build-system]\nrequires = [\"hatchling\", \"hatch-vcs\", \
             \"cython>=3.3; platform_python_implementation == 'CPython'\"]\n\
             build-backend = \"hatchling.build\"\n\n\
             [project]\nname = \"pkg\"\ndynamic = [\"version\"]\n\n\
             [tool.hatch.build.hooks.vcs]\nversion-file = \"src/_pkg_version.py\"\n\n\
             [tool.hatch.build.targets.wheel]\nsources = [\"src\"]\n",
        ),
        (
            "src/pkg/__init__.py",
            "from _pkg_version import version as __version__\n\
             try:\n    import cython\nexcept ModuleNotFoundError:\n    from pkg import _fallback as cython\n\
             if cython.compiled:\n    from cython.cimports.pkg._util import x\n\
             import zzmissing\n",
        ),
        ("src/pkg/_fallback.py", "compiled = False\n"),
    ]);
    for args in [&["--no-cache"][..], &["--no-cache", "--strict"][..]] {
        assert_eq!(
            issue_keys(&json_issues(temp.path(), args)),
            [(
                "CHK010".to_owned(),
                "src/pkg/__init__.py:zzmissing".to_owned()
            )],
            "{args:?}"
        );
    }
}

/// Pyodide provides `js` and `pyodide` itself, not an installed
/// distribution, so they are neither CHK010 nor CHK003 (#722).
#[test]
fn pyodide_runtime_modules_are_not_chk010() {
    let temp = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\n",
        ),
        (
            "src/pkg/__init__.py",
            "from pkg.contrib.emscripten import fetch\n",
        ),
        ("src/pkg/contrib/__init__.py", ""),
        ("src/pkg/contrib/emscripten/__init__.py", ""),
        (
            "src/pkg/contrib/emscripten/fetch.py",
            "import js\nfrom pyodide.ffi import to_js\nimport zzmissing\n",
        ),
    ]);
    for args in [&["--no-cache"][..], &["--no-cache", "--strict"][..]] {
        assert_eq!(
            issue_keys(&json_issues(temp.path(), args)),
            [(
                "CHK010".to_owned(),
                "src/pkg/contrib/emscripten/fetch.py:zzmissing".to_owned()
            )],
            "{args:?}"
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

/// langchain's workflows run `python .github/scripts/check_diff.py` (#734).
#[test]
fn binary_github_scripts_are_entry_roots() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [tool.chokkin]\nmode = \"app\"\nproject = [\"**/*.py\"]\n",
        ),
        (
            ".github/workflows/ci.yml",
            "jobs:\n  ci:\n    steps:\n      - run: python .github/scripts/check_diff.py\n",
        ),
        (".github/scripts/check_diff.py", "import json\n"),
        ("acme/__init__.py", ""),
        ("acme/orphan.py", ""),
    ]);
    let issues = json_issues(project.path(), &[]);
    assert_eq!(certain_chk001(&issues), ["acme/orphan.py"], "{issues:#?}");
}

#[test]
fn binary_setup_py_is_a_dev_entry_root() {
    for package_dir in ["", "acme/", "src/acme/"] {
        let helper_module = if package_dir.is_empty() {
            "_build"
        } else {
            "acme._build"
        };
        let setup = format!(
            "from setuptools import setup\nfrom {helper_module} import configure\nconfigure()\nsetup()\n"
        );
        let helper = format!("{package_dir}_build.py");
        let orphan = format!("{package_dir}orphan.py");
        let project = write_project(&[
            (
                "pyproject.toml",
                "[project]\nname = \"acme\"\nversion = \"0\"\ndependencies = []\n\n\
                 [build-system]\nrequires = [\"setuptools\"]\nbuild-backend = \"setuptools.build_meta\"\n\n\
                 [tool.chokkin]\nmode = \"app\"\n",
            ),
            ("setup.py", &setup),
            (&helper, "def configure():\n    pass\n"),
            (&orphan, ""),
            ("main.py", "print(1)\n"),
        ]);
        if !package_dir.is_empty() {
            let package = project.path().join(package_dir);
            fs::write(package.join("__init__.py"), "").expect("package");
        }
        let probe =
            chokkin::probe_project(project.path(), None, &chokkin::RuntimeOverrides::default())
                .expect("probe");
        assert!(probe.sources.files.iter().any(|file| {
            file.path == "setup.py" && file.context == chokkin::internals::FileContext::Dev
        }));
        for mode in [&[][..], &["--strict"]] {
            let issues = json_issues(project.path(), mode);
            assert_eq!(
                issue_keys(&issues),
                [("CHK001".to_owned(), orphan.clone())],
                "{package_dir} {mode:?}: {issues:#?}"
            );
        }
        let production = chokkin::probe_project(
            project.path(),
            None,
            &chokkin::RuntimeOverrides {
                production: Some(true),
                ..Default::default()
            },
        )
        .expect("production probe");
        assert!(
            production
                .sources
                .files
                .iter()
                .all(|file| file.path != "setup.py")
        );
    }
}

#[test]
fn binary_legacy_setup_py_and_workspace_setup_py_are_entry_roots() {
    let legacy = write_project(&[
        (
            "setup.py",
            "from setuptools import setup\nfrom _build import configure\nconfigure()\nsetup(name='x', py_modules=['main'])\n",
        ),
        ("_build.py", "def configure():\n    pass\n"),
        ("main.py", "print(1)\n"),
        ("orphan.py", ""),
    ]);
    for mode in [&[][..], &["--strict"]] {
        let issues = json_issues(legacy.path(), mode);
        assert_eq!(
            issue_keys(&issues),
            [("CHK001".to_owned(), "orphan.py".to_owned())],
            "{mode:?}: {issues:#?}"
        );
    }

    let workspace = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"root\"\nversion = \"0\"\n\n\
             [tool.chokkin]\nmode = \"app\"\nproject = [\"**/*.py\"]\n\n\
             [tool.uv.workspace]\nmembers = [\"core\"]\n",
        ),
        (
            "core/pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0\"\ndependencies = []\n\n\
             [build-system]\nrequires = [\"setuptools\"]\nbuild-backend = \"setuptools.build_meta\"\n",
        ),
        (
            "core/setup.py",
            "from setuptools import setup\nfrom acme._build import configure\nconfigure()\nsetup()\n",
        ),
        ("core/src/acme/__init__.py", ""),
        ("core/src/acme/_build.py", "def configure():\n    pass\n"),
        ("core/src/acme/orphan.py", ""),
        ("core/src/acme/setup.py", ""),
    ]);
    for mode in [&[][..], &["--strict"]] {
        let issues = json_issues(workspace.path(), mode);
        assert_eq!(
            issue_keys(&issues),
            [
                ("CHK001".to_owned(), "core/src/acme/orphan.py".to_owned()),
                ("CHK001".to_owned(), "core/src/acme/setup.py".to_owned()),
            ],
            "{mode:?}: {issues:#?}"
        );
    }
    assert!(
        json_issues(workspace.path(), &["--production"])
            .iter()
            .all(|issue| issue["target"] != "core/setup.py")
    );
}

#[test]
fn binary_setup_py_does_not_infer_setuptools_for_explicit_build_metadata() {
    for build_system in [
        "[build-system]\nrequires = []\nbuild-backend = \"setuptools.build_meta\"\n",
        "[build-system]\nrequires = [\"wheel\"]\n",
    ] {
        let manifest = format!(
            "[project]\nname = \"acme\"\nversion = \"0\"\ndependencies = []\n\n\
             {build_system}\n[tool.chokkin]\nmode = \"app\"\n"
        );
        let project = write_project(&[
            ("pyproject.toml", &manifest),
            ("setup.py", "from setuptools import setup\nsetup()\n"),
            ("main.py", "print(1)\n"),
        ]);
        let issues = json_issues(project.path(), &["--strict"]);
        assert_eq!(
            issue_keys(&issues),
            [("CHK003".to_owned(), "setup.py:setuptools".to_owned())],
            "{build_system}: {issues:#?}"
        );
    }
}

/// airflow's `[tool.hatch.build.targets.custom] path = "./hatch_build.py"`
/// in the root and in members, and the default hook path (#735).
#[test]
fn binary_hatch_build_scripts_are_dev_entry_roots() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n\n\
             [project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [tool.hatch.build.targets.custom]\npath = \"./hatch_build.py\"\n\n\
             [tool.chokkin]\nmode = \"app\"\nproject = [\"**/*.py\"]\n\n\
             [tool.uv.workspace]\nmembers = [\"core\"]\n",
        ),
        ("hatch_build.py", "import hatchling\n"),
        ("orphan.py", ""),
        (
            "core/pyproject.toml",
            "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n\n\
             [project]\nname = \"core\"\nversion = \"0.1.0\"\n\n\
             [project.scripts]\ncore = \"core.main:main\"\n\n\
             [tool.hatch.build.targets.wheel.hooks.custom]\n",
        ),
        ("core/hatch_build.py", "import hatchling\n"),
        ("core/src/core/__init__.py", ""),
        ("core/src/core/main.py", "def main():\n    pass\n"),
    ]);
    // `import hatchling` resolves through `[build-system].requires`, which
    // the build environment installs, so `--strict` reports no CHK003 or CHK010.
    for mode in [&[][..], &["--strict"]] {
        let issues = json_issues(project.path(), mode);
        assert_eq!(
            issue_keys(&issues),
            [("CHK001".to_owned(), "orphan.py".to_owned())],
            "{mode:?}: {issues:#?}"
        );
    }
    let production = json_issues(project.path(), &["--production"]);
    assert!(
        production.iter().all(|issue| !issue["target"]
            .as_str()
            .unwrap_or_default()
            .ends_with("hatch_build.py")),
        "{production:#?}"
    );
}

/// openai-python's `scripts/hatch_metadata.py` imports `hatchling`, which its
/// `build` group declares: the build script uses it, whatever group declares
/// it (#783).
#[test]
fn binary_build_script_imports_use_declared_groups() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[build-system]\nrequires = [\"hatchling\", \"hatch-fancy-pypi-readme\"]\n\
             build-backend = \"hatchling.build\"\n\n\
             [project]\nname = \"acme\"\nversion = \"0.1.0\"\n\
             dependencies = [\"hatch-fancy-pypi-readme\"]\n\n\
             [project.scripts]\nacme = \"acme.cli:main\"\n\n\
             [dependency-groups]\nbuild = [\"hatchling\"]\n\n\
             [tool.hatch.metadata.hooks.custom]\npath = \"scripts/hatch_metadata.py\"\n",
        ),
        (
            "scripts/hatch_metadata.py",
            "import hatch_fancy_pypi_readme\nimport hatchling\n",
        ),
        ("src/acme/__init__.py", ""),
        ("src/acme/cli.py", "def main():\n    pass\n"),
    ]);
    let issues = json_issues(project.path(), &["--strict"]);
    assert_eq!(issue_keys(&issues), [], "{issues:#?}");
}

/// openai-python's `build` group pins hatchling's build environment for
/// `[tool.uv] build-constraint-dependencies`; the pins import nothing, but
/// the same unused name in an unrelated group is still CHK002 (#819).
#[test]
fn binary_build_environment_pins_in_a_build_group_are_used() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[build-system]\nrequires = [\"hatchling==1.27.0\"]\n\
             build-backend = \"hatchling.build\"\n\n\
             [project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n\
             [project.scripts]\nacme = \"acme.cli:main\"\n\n\
             [dependency-groups]\n\
             build = [\"hatchling==1.27.0\", \"pathspec==1.1.1\", \"Pluggy==1.6.0\", \"rich\"]\n\
             lint = [\"pathspec\"]\n\
             docs = [\"pluggy\", \"trove-classifiers\"]\n\n\
             [tool.uv]\nbuild-constraint-dependencies = [\n\
             \"hatchling==1.27.0\", \"pathspec==1.1.1\", \"pluggy==1.6.0\", \"trove-classifiers\",\n]\n",
        ),
        ("src/acme/__init__.py", ""),
        ("src/acme/cli.py", "def main():\n    pass\n"),
    ]);
    let issues = json_issues(project.path(), &["--strict"]);
    let declarations: Vec<_> = issues
        .iter()
        .map(|issue| {
            (
                issue["target"].as_str().unwrap_or_default().to_owned(),
                issue["message"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let unused = |name: &str, label: &str| {
        (
            name.to_owned(),
            format!(
                "{name} is declared in dependency-groups.{label}, but no reachable import, config, or binary usage was found"
            ),
        )
    };
    assert_eq!(
        declarations,
        [
            unused("pathspec", "lint[0]"),
            unused("pluggy", "docs[0]"),
            unused("rich", "build[3]"),
            unused("trove-classifiers", "docs[1]"),
        ],
        "{issues:#?}"
    );
}

/// poetry's `poetry.masonry.api` re-exports the PEP 517 hooks for projects
/// that name it as their `build-backend`; nothing in the repository imports
/// it (#782).
#[test]
fn binary_pep517_backend_modules_are_entry_roots() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [project.scripts]\nacme = \"acme.cli:main\"\n\n\
             [tool.chokkin]\nmode = \"app\"\n",
        ),
        ("src/acme/__init__.py", ""),
        ("src/acme/cli.py", "def main():\n    pass\n"),
        (
            "src/acme/masonry/api.py",
            "from acme.core import build_sdist, build_wheel\n\n\
             __all__ = [\"build_sdist\", \"build_wheel\"]\n",
        ),
        (
            "src/acme/core.py",
            "def build_wheel(wheel_directory):\n    pass\n\n\
             def build_sdist(sdist_directory):\n    pass\n",
        ),
        (
            "src/acme/backend.py",
            "def build_wheel(wheel_directory):\n    pass\n\n\
             def build_sdist(sdist_directory):\n    pass\n\n\
             def get_requires_for_build_wheel(config_settings=None):\n    return []\n\n\
             def helper():\n    pass\n",
        ),
        (
            "src/acme/wheel_only.py",
            "def build_wheel(wheel_directory):\n    pass\n",
        ),
    ]);
    // The frontend calls the hooks, so they are no CHK006 either.
    let issues = json_issues(project.path(), &[]);
    assert_eq!(
        issue_keys(&issues),
        [
            ("CHK001".to_owned(), "src/acme/wheel_only.py".to_owned()),
            ("CHK006".to_owned(), "src/acme/backend.py:helper".to_owned()),
        ],
        "{issues:#?}"
    );
}

/// A backend package that re-exports its hooks from `__init__.py`: the
/// frontend reads them from the package, so they are no CHK007, even the
/// optional hooks `__all__` leaves out (#873).
#[test]
fn binary_pep517_backend_package_reexports_are_not_chk007() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [project.scripts]\nacme = \"acme.cli:main\"\n\n\
             [tool.chokkin]\nmode = \"app\"\n",
        ),
        ("src/acme/__init__.py", ""),
        ("src/acme/cli.py", "def main():\n    pass\n"),
        (
            "src/acme/backend/__init__.py",
            "from ._impl import build_sdist, build_wheel, helper, \
             prepare_metadata_for_build_wheel\n\n\
             __all__ = [\"build_sdist\", \"build_wheel\"]\n",
        ),
        (
            "src/acme/backend/_impl.py",
            "def build_wheel(wheel_directory):\n    pass\n\n\
             def build_sdist(sdist_directory):\n    pass\n\n\
             def prepare_metadata_for_build_wheel(metadata_directory):\n    pass\n\n\
             def helper():\n    pass\n",
        ),
    ]);
    let issues = json_issues(project.path(), &[]);
    assert_eq!(
        issue_keys(&issues),
        [(
            "CHK007".to_owned(),
            "src/acme/backend/__init__.py:helper".to_owned()
        )],
        "{issues:#?}"
    );
}

/// An in-tree backend under `backend-path` runs in the build environment, so
/// its imports resolve through `[build-system].requires`, and the frontend
/// calls its hooks (#782).
#[test]
fn binary_in_tree_backend_imports_resolve_through_build_requires() {
    for backend_path in ["_custom_build", "src"] {
        let pyproject = format!(
            "[build-system]\nrequires = [\"setuptools\", \"tomli\"]\n\
             build-backend = \"backend\"\nbackend-path = [\"{backend_path}\"]\n\n\
             [project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [project.scripts]\nacme = \"acme.cli:main\"\n\n\
             [tool.chokkin]\nmode = \"app\"\n"
        );
        let backend = format!("{backend_path}/backend.py");
        let project = write_project(&[
            ("pyproject.toml", pyproject.as_str()),
            ("src/acme/__init__.py", ""),
            ("src/acme/cli.py", "def main():\n    pass\n"),
            (
                backend.as_str(),
                "import tomli\nfrom setuptools import build_meta\n\n\
                 def build_wheel(wheel_directory):\n    return build_meta.build_wheel(wheel_directory)\n\n\
                 def build_sdist(sdist_directory):\n    return build_meta.build_sdist(sdist_directory)\n",
            ),
        ]);
        let issues = json_issues(project.path(), &[]);
        assert!(
            issue_keys(&issues).is_empty(),
            "{backend_path}: {issues:#?}"
        );
    }
}

/// #731: a workspace member's own `pytest11` entry point makes its plugin
/// module pytest-loaded; its other modules still need what they import.
#[test]
fn binary_member_pytest_plugin_imports_pytest_as_info() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"workspace-root\"\nversion = \"0.1.0\"\n\n\
             [project.scripts]\ntools-cli = \"tools.cli:main\"\n\n\
             [tool.uv.workspace]\nmembers = [\"packages/*\"]\n",
        ),
        (
            "packages/tools/pyproject.toml",
            "[project]\nname = \"tools\"\nversion = \"0.1.0\"\n\n\
             [project.entry-points.pytest11]\ntools = \"tools.plugin\"\n\n\
             [dependency-groups]\ndev = [\"pytest\", \"ipython\"]\n",
        ),
        ("packages/tools/src/tools/__init__.py", ""),
        (
            "packages/tools/src/tools/plugin.py",
            "import pytest\n\n\n@pytest.fixture\ndef tool():\n    return 1\n",
        ),
        (
            "packages/tools/src/tools/cli.py",
            "import IPython\n\n\ndef main():\n    IPython.embed()\n",
        ),
    ]);
    let misplaced: Vec<(String, String)> = json_issues(project.path(), &["--strict"])
        .iter()
        .filter(|issue| issue["code"] == "CHK005")
        .map(|issue| {
            let field = |name: &str| issue[name].as_str().unwrap_or_default().to_owned();
            (field("target"), field("severity"))
        })
        .collect();
    assert_eq!(
        misplaced,
        [
            ("tools:ipython".to_owned(), "warning".to_owned()),
            ("tools:pytest".to_owned(), "info".to_owned()),
        ]
    );
}

/// #808: a function-local import of a package the member guards with
/// `except ImportError` or names in an `is_x…` predicate is the same optional
/// feature, and test support the package ships runs only under a test suite.
#[test]
fn binary_deferred_imports_of_optional_packages_are_info() {
    let root = fixture_path(&["deps", "misplaced_deferred_optional"]);
    let mut misplaced: Vec<(String, String, String)> = json_issues(&root, &["--strict"])
        .iter()
        .filter(|issue| issue["code"] == "CHK005")
        .map(|issue| {
            let field = |name: &str| issue[name].as_str().unwrap_or_default().to_owned();
            (field("target"), field("severity"), field("confidence"))
        })
        .collect();
    misplaced.sort();
    let expected = |target: &str, severity: &str| {
        (target.to_owned(), severity.to_owned(), "likely".to_owned())
    };
    // Only frames' tests check plotly; frames' `is_openai_v1` says nothing about chat;
    // the client loads chat's test support at startup. A platform guard alone
    // makes uvloop required there, while colorama is also under `try`.
    assert_eq!(
        misplaced,
        [
            expected("chat:openai", "warning"),
            (
                "chat:responses".to_owned(),
                "warning".to_owned(),
                "certain".to_owned()
            ),
            expected("frames:colorama", "info"),
            expected("frames:cryptography", "info"),
            expected("frames:plotly", "warning"),
            expected("frames:polars", "info"),
            expected("frames:selenium", "info"),
            expected("frames:uvloop", "warning"),
        ]
    );
}

#[test]
fn binary_command_run_scripts_import_dependency_groups_as_dev() {
    // airflow's pre-commit runs `task-sdk/dev/generate_task_sdk_models.py`,
    // which imports its `codegen` group (#797), even with a `__main__` block
    // unlike a task file's script (#912). The package module run by the
    // same workflow keeps its runtime context.
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[dependency-groups]\ncodegen = [\"gen-lib\", \"pkg-lib\"]\n\n[tool.chokkin]\nproject = [\"**/*.py\"]\n",
        ),
        (
            ".github/workflows/ci.yml",
            "jobs:\n  gen:\n    steps:\n      - run: python tools/gen.py\n      - run: python acme/cli.py\n",
        ),
        (
            "tools/gen.py",
            "import gen_lib\n\nif __name__ == \"__main__\":\n    gen_lib.run()\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/cli.py", "import pkg_lib\n\npkg_lib.run()\n"),
    ]);
    for mode in [&[][..], &["--strict"]] {
        let keys = issue_keys(&json_issues(project.path(), mode));
        assert!(
            !keys.iter().any(|(_, target)| target.contains("gen")),
            "{keys:?}"
        );
        if mode.is_empty() {
            continue;
        }
        assert!(
            keys.contains(&("CHK005".to_owned(), "pkg-lib".to_owned())),
            "{keys:?}"
        );
    }
}

/// A file another root runs in production keeps its context when a command
/// also runs it, so `--production` still analyzes the app (#797): the root
/// `main.py`, a module a console script names and a `FLASK_APP` file (their
/// dev-group imports are misplaced), a configured entry, and a server app a
/// Makefile serves.
#[test]
fn binary_command_run_runtime_roots_keep_runtime_context() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"flask\", \"gunicorn\", \"httpx\", \"requests\", \"rich\"]\n\n[dependency-groups]\ndev = [\"attrs\", \"click\"]\n\n[project.scripts]\nacme = \"cli:main\"\n\n[tool.chokkin]\nproject = [\"*.py\"]\nentry = [\"serve.py\"]\n",
        ),
        (
            ".github/workflows/ci.yml",
            "jobs:\n  run:\n    steps:\n      - run: python main.py\n      - run: python cli.py\n      - run: python serve.py\n      - run: python hello.py\n      - run: python web.py\n",
        ),
        (".flaskenv", "FLASK_APP=hello.py\n"),
        (
            "hello.py",
            "import attrs\nimport flask\n\napp = flask.Flask(__name__)\n",
        ),
        ("Makefile", "serve:\n\tgunicorn web:app\n"),
        ("web.py", "import httpx\n\napp = httpx.Client()\n"),
        ("main.py", "import helper\nimport requests\n"),
        ("helper.py", ""),
        (
            "cli.py",
            "import click\n\n\ndef main():\n    click.echo()\n",
        ),
        ("serve.py", "import rich\n"),
    ]);
    for mode in [&["--strict"][..], &["--strict", "--production"]] {
        let keys = issue_keys(&json_issues(project.path(), mode));
        assert_eq!(
            keys,
            [
                ("CHK005".to_owned(), "attrs".to_owned()),
                ("CHK005".to_owned(), "click".to_owned())
            ],
            "{mode:?}: {keys:?}"
        );
    }
}

/// Apps named by `flask --app` / `celery -A` in `scripts/` and `bin/` shell
/// scripts are entry roots, so only the control module is unused. The
/// `*_bare_module` fixtures name a module without `:attr`, whose app the tool
/// finds by name, so it is no unused export either (#851); `flask_app_paths`
/// and `flask_root_app` name them by file path (#866).
#[test]
fn binary_shell_script_apps_are_entry_roots() {
    for (fixture, unused) in [
        ("flask_shell_scripts", "src/myapp/orphan.py"),
        ("celery_shell_scripts", "src/worker/orphan.py"),
        ("flask_bare_module", "src/web/orphan.py"),
        ("celery_bare_module", "src/worker/orphan.py"),
        ("flask_app_paths", "src/web/orphan.py"),
        ("flask_root_app", "orphan.py"),
    ] {
        let keys = issue_keys(&json_issues(
            &fixture_path(&["plugins", fixture]),
            &["--strict"],
        ));
        assert_eq!(
            keys,
            [("CHK001".to_owned(), unused.to_owned())],
            "{fixture}: {keys:?}"
        );
    }
}

/// A `FLASK_APP` file under `scripts/` is a dev entry root, so a library
/// serving a dev app stays a library and keeps its exports (#866).
#[test]
fn binary_flask_dev_app_paths_keep_library_mode() {
    let keys = issue_keys(&json_issues(
        &fixture_path(&["plugins", "flask_dev_app"]),
        &["--strict"],
    ));
    assert_eq!(keys, [], "{keys:?}");
}

/// stderr of `--trace <target>` on the `trace_origins` fixture, which has an
/// entry root of every origin kind.
fn trace_stderr(target: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-exit-code", "--trace", target])
        .arg(fixture_path(&["reachability", "trace_origins"]))
        .output()
        .expect("run chokkin");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stderr).expect("utf8")
}

#[test]
fn binary_trace_prints_path_to_reachable_file() {
    assert_eq!(
        trace_stderr("src/acme/util.py"),
        "Trace path:\n├─ file src/acme/cli.py\n└─ import acme.util (line 3)\n\n"
    );
    // `./` and `\` spellings name the same root-relative file.
    assert_eq!(
        trace_stderr("./src/acme/dyn.py"),
        "Trace path:\n├─ file src/acme/cli.py\n└─ dynamic import acme.dyn (line 5)\n\n"
    );
    assert_eq!(
        trace_stderr(r"src\acme\views.py"),
        "Trace path:\n└─ plugin ref acme.views (flask route decorator)\n\n"
    );
}

#[test]
fn binary_trace_reports_undiscovered_file() {
    assert_eq!(
        trace_stderr("src/acme/missing.py"),
        "trace: file `src/acme/missing.py` was not discovered in this project\n\n"
    );
}

#[test]
fn binary_trace_explains_unreachable_file() {
    assert_eq!(
        trace_stderr("src/acme/legacy.py"),
        "\
Negative trace for src/acme/legacy.py:

  reason: not reachable from any entry root

  entry roots analyzed:
    ├─ src/acme/__main__.py (auto: __main__.py)
    ├─ src/acme/backend.py (build backend: PEP 517)
    ├─ src/acme/cli.py (config)
    ├─ src/acme/main.py (manifest: [console].acme)
    ├─ src/acme/web.py (symbol: FLASK_APP)
    ├─ src/tools/gen.py (command: workflow run (.github/workflows/ci.yml:6))
    ├─ src/tools/report.py (script: PEP 723)
    └─ tests/test_smoke.py (plugin: pytest (pytest defaults))

  incoming imports (all unreachable):
    ├─ src/acme/compat.py:1 imports acme.legacy (file also unreachable)
    └─ src/acme/old_api.py:1 imports acme.legacy (file also unreachable)

  suggestion: add to [tool.chokkin].entry or verify import chain

"
    );
}

/// A warning that names a file is annotated on it, so a workflow run shows
/// it on the PR diff (#895).
#[test]
fn binary_github_annotates_warnings_on_their_file() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/broken.py", "import os\ndef broken(:\n"),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "github"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(
        stdout.lines().any(|line| line.starts_with(
            "::warning file=acme/broken.py,line=2,title=chokkin::parse: skipped `acme/broken.py`: syntax error at line 2"
        )),
        "{stdout}"
    );
}

/// A root `.gitignore` line that does not parse is one warning naming the
/// file, though the root walk and every member walk meet it (#903).
#[test]
fn binary_unparsable_root_gitignore_warns_once_in_a_workspace() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [tool.uv.workspace]\nmembers = [\"packages/*\"]\n",
        ),
        (".gitignore", "bad{glob\n"),
        ("acme/__init__.py", ""),
        (
            "packages/core/pyproject.toml",
            "[project]\nname = \"core\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        ("packages/core/core/__init__.py", ""),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    let sources: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("sources:"))
        .collect();
    let gitignore = project
        .path()
        .canonicalize()
        .expect("canonical root")
        .join(".gitignore");
    let expected = format!(
        "sources: could not read `{}`: line 1: error parsing glob 'bad{{glob'",
        gitignore.display()
    );
    assert_eq!(sources.len(), 1, "{stderr}");
    assert!(sources[0].starts_with(&expected), "{expected}\n{stderr}");
}

/// A `.gitignore` below the root that cannot be read is one warning naming
/// it, though the root walk and the member walk both meet it, and the text
/// reporter prints it as JSON does (#915).
#[test]
fn binary_unreadable_nested_gitignore_warns_once_in_a_workspace() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [tool.uv.workspace]\nmembers = [\"packages/*\"]\n",
        ),
        ("acme/__init__.py", ""),
        (
            "packages/core/pyproject.toml",
            "[project]\nname = \"core\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        ("packages/core/core/__init__.py", ""),
        ("packages/core/core/gen/out.py", ""),
    ]);
    fs::write(
        project.path().join("packages/core/core/gen/.gitignore"),
        b"out.py\n\xff\n",
    )
    .expect("write gitignore");

    for reporter in ["json", "default"] {
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--reporter", reporter])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        let sources: Vec<&str> = stderr
            .lines()
            .filter(|line| line.contains("sources:"))
            .collect();
        assert_eq!(
            sources,
            [
                "sources: could not read `packages/core/core/gen/.gitignore`; \
              its ignore rules may be missing"
            ],
            "{reporter}: {stderr}"
        );
    }
}

/// #917: a `requires-python` that is not a specifier set falls back to py311,
/// and says so; an explicit `target_version` makes it moot.
#[test]
fn binary_unreadable_requires_python_warns_unless_target_version_is_set() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"pkg\"\nversion = \"0\"\nrequires-python = \">=3.12,bogus\"\n",
        ),
        ("src/pkg/__init__.py", "type Alias = int\n"),
        (
            "scripts/tool.py",
            "# /// script\n# requires-python = \",\"\n# dependencies = []\n# ///\nimport os\n",
        ),
    ]);
    let manifest_lines = |root: &std::path::Path| {
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--reporter", "json"])
            .arg(root)
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        stderr
            .lines()
            .filter(|line| line.contains("requires-python"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        manifest_lines(project.path()),
        [
            "manifest: could not read `requires-python` \">=3.12,bogus\" in `pyproject.toml`; \
             assuming target version py311; imports of modules only other versions ship as \
             stdlib are reported as hints",
            "manifest: could not read `requires-python` \",\" in the PEP 723 block of \
             `scripts/tool.py`; using the project's target version; its imports of modules \
             only other versions ship as stdlib are reported as hints",
        ]
    );

    fs::write(
        project.path().join("pyproject.toml"),
        "[project]\nname = \"pkg\"\nversion = \"0\"\nrequires-python = \">=3.12,bogus\"\n\n\
         [tool.chokkin]\ntarget_version = \"py312\"\n",
    )
    .expect("write pyproject");
    assert_eq!(
        manifest_lines(project.path()),
        [
            "manifest: could not read `requires-python` \",\" in the PEP 723 block of \
          `scripts/tool.py`; using the project's target version; its imports of modules \
          only other versions ship as stdlib are reported as hints"
        ]
    );
}

/// Text from the analyzed project, here a file name and venv METADATA, reaches
/// text reporters and stderr with control characters escaped, so it cannot
/// colour the terminal or forge a line; a dist-info whose `Name:` is not a
/// PEP 508 name is ignored with a warning (#973).
#[cfg(unix)]
#[test]
fn binary_escapes_control_characters_from_the_project() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"pyyaml\"]\n\n\
             [project.scripts]\nacme = \"acme:main\"\n\n[tool.chokkin]\nmode = \"app\"\n",
        ),
        ("acme/__init__.py", "import yaml\n\ndef main():\n    pass\n"),
        ("acme/bad\x1b[31m\nCHK001 forged.py", ""),
        (
            ".venv/lib/python3.11/site-packages/evil-1.0.dist-info/METADATA",
            "Name: evil\x1b[31mRED\x1b[0m\n",
        ),
        (
            ".venv/lib/python3.11/site-packages/evil-1.0.dist-info/top_level.txt",
            "yaml\n",
        ),
    ]);
    let control = |text: &str| text.chars().any(|ch| ch.is_control() && ch != '\n');

    for reporter in ["default", "compact", "markdown", "github"] {
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--reporter", reporter])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stdout = String::from_utf8(output.stdout).expect("utf8");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        assert!(!control(&stdout), "{reporter}: {stdout:?}");
        assert!(!control(&stderr), "{reporter}: {stderr:?}");
        assert!(
            stdout.contains("bad\\u{1b}[31m\\u{a}CHK001 forged.py"),
            "{reporter}: {stdout}"
        );
        assert!(
            !stdout.lines().any(|line| line.starts_with("CHK001 forged")),
            "{reporter}: {stdout}"
        );
        assert!(
            stderr.lines().any(|line| line
                == "resolve: ignored `.venv/lib/python3.11/site-packages/evil-1.0.dist-info`: \
                    `evil\\u{1b}[31mRED\\u{1b}[0m` is not a PEP 508 distribution name"),
            "{reporter}: {stderr}"
        );
    }

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let issues = parsed["issues"].to_string();
    assert!(!issues.contains("evil"), "{issues}");
    assert!(!issues.contains("\"CHK002\""), "{issues}");
    assert!(!issues.contains("\"CHK003\""), "{issues}");
}

/// A character the text outputs must spell out: controls other than the line
/// break, bidi embedding/override/isolate and line/paragraph separators.
fn unescaped(ch: char) -> bool {
    ch != '\n'
        && (ch.is_control()
            || matches!(
                ch,
                '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}'
            ))
}

/// The other outputs that echo the analyzed project — `--probe`, `--trace`,
/// `--explain`, a fatal error quoting the manifest and the baseline message —
/// escape control characters too, and bidi controls and line separators along
/// with them, which reorder or break a terminal line (#996).
#[cfg(unix)]
#[test]
fn binary_escapes_control_characters_outside_the_reporters() {
    const FORGED: &str = "acme/bad\x1b[31m\nCHK001 forged.py";
    let project = write_project(&[
        (
            "pyproject.toml",
            "[build-system]\nrequires = []\nbuild-backend = \"evil\\u001b[31m\"\n\n\
             [project]\nname = \"acme\\u2028Project : forged\"\nversion = \"0.1.0\"\n\
             dependencies = []\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"acme/__init__.py\"]\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/orphan.py", ""),
        (FORGED, "import acme.orphan\n"),
        (
            "scripts/run\u{202e}yp.py",
            "# /// script\n# requires-python = \">=3.11\\u202e\"\n# dependencies = []\n# ///\n",
        ),
    ]);
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .arg("--no-cache")
            .args(args)
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stdout = String::from_utf8(output.stdout).expect("utf8");
        let stderr = String::from_utf8(output.stderr).expect("utf8");
        for text in [&stdout, &stderr] {
            assert!(!text.chars().any(unescaped), "{args:?}: {text:?}");
        }
        (stdout, stderr)
    };

    let (probe, _) = run(&["--probe"]);
    assert!(
        probe.contains("Project : acme\\u{2028}Project : forged\n"),
        "{probe}"
    );
    assert!(
        probe.contains("build backend    : evil\\u{1b}[31m"),
        "{probe}"
    );
    assert!(
        probe.contains(
            "scripts/run\\u{202e}yp.py (0 dependencies, requires-python >=3.11\\u{202e})"
        ),
        "{probe}"
    );

    let (_, trace) = run(&["--trace", FORGED]);
    assert!(
        trace.contains("Negative trace for acme/bad\\u{1b}[31m\\u{a}CHK001 forged.py:"),
        "{trace}"
    );
    let (_, trace) = run(&["--trace", "acme/orphan.py"]);
    assert!(
        trace.contains("acme/bad\\u{1b}[31m\\u{a}CHK001 forged.py:1 imports acme.orphan"),
        "{trace}"
    );

    let (_, explain) = run(&["--explain", &format!("CHK001:{FORGED}")]);
    assert!(
        explain.contains("acme/bad\\u{1b}[31m\\u{a}CHK001 forged.py"),
        "{explain}"
    );

    let (_, explain) = run(&["--explain", "CHK001:none\u{1b}[31m.py"]);
    assert!(
        explain.contains("no matching issue for `CHK001:none\\u{1b}[31m.py`"),
        "{explain}"
    );

    let (_, baseline) = run(&["--baseline", "base\u{1b}[31m.json", "--update-baseline"]);
    assert!(baseline.contains("to base\\u{1b}[31m.json"), "{baseline}");

    fs::write(
        project.path().join("pyproject.toml"),
        "[project]\nname = \"acme\"\n\x1b[31m = 1\n",
    )
    .expect("write pyproject");
    let (_, error) = run(&["--probe"]);
    assert!(error.contains("\\u{1b}[31m"), "{error}");
    assert!(error.lines().count() > 1, "{error}");
}

/// locust's shape (#977): the wheel includes `pytest_locust/`, a namespace
/// directory with only `plugin.py`, named by a `pytest11` entry point. It is
/// a package, so the entry maps and the plugin is neither unused nor its
/// import unresolved.
#[test]
fn binary_wheel_included_namespace_directory_is_a_package() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n\n\
             [project]\nname = \"locust\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [project.entry-points.pytest11]\nlocust = \"pytest_locust.plugin\"\n\n\
             [tool.hatch.build.targets.wheel]\ninclude = [\"locust\", \"pytest_locust\"]\n",
        ),
        ("locust/__init__.py", "from locust.core import run\n"),
        ("locust/core.py", "def run():\n    return 1\n"),
        (
            "pytest_locust/plugin.py",
            "from locust import run\n\n\ndef pytest_configure(config):\n    run()\n",
        ),
        ("locust/helpers.py", "import pytest_locust.plugin\n"),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(!stderr.contains("could not map"), "{stderr}");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let issues = parsed["issues"].as_array().expect("issues");
    let named: Vec<String> = issues
        .iter()
        .filter(|issue| issue.to_string().contains("pytest_locust"))
        .map(ToString::to_string)
        .collect();
    assert_eq!(named, Vec::<String>::new());
}

#[test]
fn pytest_testpaths_suite_package_resolves_by_absolute_name() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"celery\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [tool.pytest.ini_options]\ntestpaths = \"t/unit/\"\n",
        ),
        ("celery/__init__.py", "def run():\n    return 1\n"),
        ("t/__init__.py", ""),
        ("t/unit/__init__.py", ""),
        ("t/unit/helpers.py", "def helper():\n    return 1\n"),
        (
            "t/unit/test_run.py",
            "from celery import run\nfrom t.unit.helpers import helper\nfrom .helpers import helper as again\n\n\
             def test_run():\n    assert run() == helper() == again()\n",
        ),
        ("t/integration/__init__.py", ""),
        (
            "t/integration/test_live.py",
            "from t.unit.helpers import helper\nfrom . import conftest\n",
        ),
        ("t/integration/conftest.py", "VALUE = 1\n"),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(!stderr.contains("relative import"), "{stderr}");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let issues: Vec<String> = parsed["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .filter(|issue| ["CHK001", "CHK010"].contains(&issue["code"].as_str().unwrap_or("")))
        .map(ToString::to_string)
        .collect();
    assert_eq!(issues, Vec::<String>::new());
}

#[test]
fn unused_dependency_names_the_distribution_and_its_pyproject_line() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\n  \"requests>=2\",\n  \"rich\",\n]\n\n\
             [project.optional-dependencies]\ncli = [\"click\"]\n\n\
             [dependency-groups]\ndev = [\n  \"attrs\",\n]\n\n\
             [tool.uv]\ndev-dependencies = [\"ruff\"]\n",
        ),
        ("acme/__init__.py", "import rich\n"),
    ]);
    let issues = json_issues(project.path(), &["--strict"]);
    let unused: Vec<_> = issues
        .iter()
        .filter(|issue| issue["code"] == "CHK002")
        .map(|issue| {
            (
                issue["message"].as_str().unwrap_or_default().to_owned(),
                issue["manifest"]["line"].as_u64(),
            )
        })
        .collect();
    let expected = |name: &str, label: &str, line: u64| {
        (
            format!(
                "{name} is declared in {label}, but no reachable import, config, or binary usage was found"
            ),
            Some(line),
        )
    };
    assert_eq!(
        unused,
        [
            expected("attrs", "dependency-groups.dev[0]", 14),
            expected("click", "project.optional-dependencies.cli[0]", 10),
            expected("requests", "project.dependencies[0]", 5),
            expected("ruff", "tool.uv.dev-dependencies[0]", 18),
        ],
        "{issues:#?}"
    );
}

#[test]
fn poetry_dependency_lines_come_from_their_key() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[tool.poetry]\nname = \"acme\"\nversion = \"0.1.0\"\n\n\
             [tool.poetry.dependencies]\npython = \"^3.10\"\n\"zope.interface\" = \"*\"\nrich = { version = \"*\" }\n",
        ),
        ("acme/__init__.py", "import rich\n"),
    ]);
    let issues = json_issues(project.path(), &[]);
    let lines: Vec<_> = issues
        .iter()
        .filter(|issue| issue["code"] == "CHK002")
        .map(|issue| {
            (
                issue["target"].as_str().unwrap_or_default().to_owned(),
                issue["manifest"]["line"].as_u64(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        [("zope-interface".to_owned(), Some(7))],
        "{issues:#?}"
    );
}

/// A suite inside a package (`core/tests`) leaves the package a guess
/// candidate: widening it to `core` would make `core/api.py` test code.
#[test]
fn pytest_testpaths_inside_a_package_keeps_the_package_runtime() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"service\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [tool.pytest.ini_options]\ntestpaths = [\"core/tests\"]\n",
        ),
        ("core/__init__.py", ""),
        ("core/api.py", "def handler():\n    return 1\n"),
        ("core/tests/__init__.py", ""),
        (
            "core/tests/test_api.py",
            "from core.api import handler\n\ndef test_handler():\n    assert handler() == 1\n",
        ),
        ("utils/__init__.py", ""),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--probe"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.contains("packages: core"), "{stdout}");
}

/// A file with an `if __name__ == "__main__":` block is run as a script, so
/// it and what it imports there are reachable, wherever it sits (#1026).
/// `--trace` names the rule. A guard the parser does not read
/// (`__name__ in (...)`) leaves the file an orphan.
#[test]
fn binary_main_block_files_are_entry_roots() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [project.scripts]\nacme = \"acme.cli:main\"\n\n\
             [tool.chokkin]\nmode = \"app\"\n",
        ),
        ("src/acme/__init__.py", ""),
        ("src/acme/cli.py", "def main():\n    pass\n"),
        (
            "src/acme/contrib/example.py",
            "def run():\n    pass\n\n\nif __name__ == \"__main__\":\n    \
             from acme.contrib.support import helper\n\n    helper()\n    run()\n",
        ),
        ("src/acme/contrib/support.py", "def helper():\n    pass\n"),
        (
            "src/acme/contrib/odd_guard.py",
            "if __name__ in (\"__main__\",):\n    pass\n",
        ),
        (
            "src/acme/contrib/nested.py",
            "def main():\n    if __name__ == \"__main__\":\n        pass\n",
        ),
        ("src/acme/contrib/orphan.py", "def unused():\n    pass\n"),
    ]);
    let issues = json_issues(project.path(), &[]);
    assert_eq!(
        issue_keys(&issues),
        [
            ("CHK001".to_owned(), "src/acme/contrib/nested.py".to_owned()),
            (
                "CHK001".to_owned(),
                "src/acme/contrib/odd_guard.py".to_owned()
            ),
            ("CHK001".to_owned(), "src/acme/contrib/orphan.py".to_owned()),
        ],
        "{issues:#?}"
    );

    let trace = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--trace", "src/acme/contrib/orphan.py"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(trace.stderr).expect("utf8");
    assert!(
        stderr.contains("src/acme/contrib/example.py (auto: main-block)"),
        "{stderr}"
    );
}

/// A Dockerfile below the root serves its app too (#1061): the module
/// resolves from the root first, then from the Dockerfile's directory, the
/// usual build context. A Dockerfile the gitignore drops is not read.
#[test]
fn binary_nested_dockerfile_apps_are_entry_roots() {
    let cmd = "FROM python\nWORKDIR /code\nCOPY ./app /code/app\n\
               CMD [\"uvicorn\", \"app.main:app\", \"--host\", \"0.0.0.0\"]\n";
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n\
             [project.scripts]\nacme = \"acme.cli:main\"\n\n\
             [tool.chokkin]\nmode = \"app\"\n\
             project = [\"acme/**/*.py\", \"dev/**/*.py\", \"other/**/*.py\"]\n",
        ),
        (".gitignore", "other/Dockerfile\n"),
        ("acme/__init__.py", ""),
        ("acme/cli.py", "def main():\n    pass\n"),
        ("acme/wsgi.py", "application = object()\n"),
        ("deploy/Dockerfile", "FROM python\nCMD gunicorn acme.wsgi\n"),
        ("dev/sim/Dockerfile", cmd),
        ("dev/sim/app/__init__.py", ""),
        ("dev/sim/app/main.py", "app = object()\n"),
        ("other/Dockerfile", cmd),
        ("other/app/__init__.py", ""),
        ("other/app/main.py", "app = object()\n"),
        ("skip/Dockerfile", cmd),
        ("skip/app/__init__.py", ""),
        ("skip/app/main.py", "app = object()\n"),
    ]);
    let issues = json_issues(project.path(), &["--no-cache"]);
    assert_eq!(
        issue_keys(&issues),
        [("CHK001".to_owned(), "other/app/main.py".to_owned())],
        "{issues:#?}"
    );

    // An app beside its Dockerfile that discovery left out is a skipped read.
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--no-exit-code"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains("entry: skipped entry `skip/app/main.py`: not a discovered source file"),
        "{stderr}"
    );

    // The orphan's negative trace lists the entry roots with their origins.
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args([
            "--no-cache",
            "--no-exit-code",
            "--trace",
            "other/app/main.py",
        ])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    for root in [
        "acme/wsgi.py (command: CMD (deploy/Dockerfile:2))",
        "dev/sim/app/main.py (command: CMD (dev/sim/Dockerfile:4))",
    ] {
        assert!(stderr.contains(root), "{root}\n{stderr}");
    }
}

/// An app found only beside its nested Dockerfile runs in the image's own
/// tree, whose installs and layout are not read: the imports under that
/// directory are hints, while an app the root resolves keeps them (#1061).
/// An app beside a `.py` file is no image, so its tree keeps them too.
#[test]
fn binary_nested_dockerfile_tree_imports_are_hints() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"uvicorn\"]\n\n\
             [tool.chokkin]\nmode = \"app\"\nproject = [\"acme/**/*.py\", \"sim/**/*.py\"]\n",
        ),
        ("acme/__init__.py", ""),
        (
            "acme/wsgi.py",
            "import httpx\nimport uvicorn\napplication = object()\nuvicorn.run(\"tool.main:app\")\n",
        ),
        ("acme/tool/__init__.py", ""),
        ("acme/tool/main.py", "import rich\napp = object()\n"),
        ("deploy/Dockerfile", "FROM python\nCMD gunicorn acme.wsgi\n"),
        (
            "sim/Dockerfile",
            "FROM python\nRUN pip install -r requirements.txt\nCOPY ./app /code/app\n\
             CMD uvicorn app.main:app\n",
        ),
        ("sim/requirements.txt", "fastapi\n"),
        ("sim/app/__init__.py", ""),
        (
            "sim/app/main.py",
            "import fastapi\nfrom app import models\napp = fastapi.FastAPI()\n",
        ),
        ("sim/app/models.py", ""),
    ]);
    let issues = json_issues(project.path(), &["--no-cache"]);
    assert_eq!(
        issue_keys(&issues),
        [
            ("CHK003".to_owned(), "acme/tool/main.py:rich".to_owned()),
            ("CHK003".to_owned(), "acme/wsgi.py:httpx".to_owned()),
        ],
        "{issues:#?}"
    );
}

/// A `__main__` block in a deep `app.py` does not make a library an app:
/// the rule joins after mode resolution, which reads file names only (#1026).
#[test]
fn binary_main_block_does_not_change_project_mode() {
    let project = write_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n",
        ),
        ("src/acme/__init__.py", "from acme import core\n"),
        ("src/acme/core.py", "def api():\n    pass\n"),
        (
            "src/acme/tools/app.py",
            "if __name__ == \"__main__\":\n    print(1)\n",
        ),
        ("src/acme/tools/spare.py", "def spare():\n    pass\n"),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(report["mode"], "library", "{stdout}");
    // A library's orphans are `maybe`, below the default floor.
    let orphans: Vec<_> = json_issues(project.path(), &["--strict"])
        .iter()
        .filter(|issue| issue["code"] == "CHK001")
        .map(|issue| issue["target"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(orphans, ["src/acme/tools/spare.py"]);
}

/// A dotted string literal that resolves to a project file may load it by
/// name (airflow's `lazy_load_command("airflow.cli.commands.x.y")`,
/// gunicorn's `"python:pkg.config"`, celery's `"pkg.backends.redis:Backend"`),
/// so that file's CHK001 falls to an info note at likely, and `--explain`
/// names the string (#1026, #493). A computed
/// string is not recognized and lowers nothing.
#[test]
fn binary_dotted_string_literal_lowers_the_named_file() {
    let pyproject = "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[project.scripts]\nacme = \"acme.cli:main\"\n\n[tool.chokkin]\nmode = \"app\"\n";
    let project = write_project(&[
        ("pyproject.toml", pyproject),
        ("acme/__init__.py", ""),
        (
            "acme/cli.py",
            "from acme.loader import lazy_load\nCOMMANDS = {\"cheat\": lazy_load(\"acme.commands.cheat_sheet.run\")}\nGUNICORN = {\"config\": \"python:acme.gunicorn_config\"}\nNAME = \"computed\"\nDYNAMIC = f\"acme.commands.{NAME}\"\nBACKENDS = {\"redis\": \"acme.backends.redis:RedisBackend\"}\n\n\ndef main():\n    return COMMANDS, GUNICORN, DYNAMIC, BACKENDS\n",
        ),
        ("acme/loader.py", "def lazy_load(path):\n    return path\n"),
        ("acme/commands/__init__.py", ""),
        ("acme/commands/cheat_sheet.py", "def run():\n    pass\n"),
        ("acme/commands/computed.py", "WIDTH = 80\n"),
        ("acme/gunicorn_config.py", "workers = 2\n"),
        ("acme/backends/__init__.py", ""),
        ("acme/backends/redis.py", "class RedisBackend:\n    pass\n"),
        ("acme/orphan.py", ""),
    ]);

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--reporter", "json"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let unused: Vec<(String, String, String, String)> = issue_levels(project.path())
        .into_iter()
        .filter(|(code, ..)| code == "CHK001")
        .collect();
    let level = |target: &str, severity: &str, confidence: &str| {
        (
            "CHK001".to_owned(),
            target.to_owned(),
            severity.to_owned(),
            confidence.to_owned(),
        )
    };
    assert_eq!(
        unused,
        [
            level("acme/backends/redis.py", "info", "likely"),
            level("acme/commands/cheat_sheet.py", "info", "likely"),
            level("acme/commands/computed.py", "warning", "certain"),
            level("acme/gunicorn_config.py", "info", "likely"),
            level("acme/orphan.py", "warning", "certain"),
        ]
    );
    // The gap reaches the user without `--explain` (ADR 0005 §3).
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains(
            "parse: `acme/cli.py:2` names `acme.commands.cheat_sheet.run` and 2 more modules in strings that may load files no entry reaches; those files are reported as unused at info, if at all"
        ),
        "{stderr}"
    );

    for (target, reason) in [
        (
            "CHK001:acme/commands/cheat_sheet.py",
            "a string literal names the module, which may be loaded by name: `acme.commands.cheat_sheet.run` in `acme/cli.py:2`",
        ),
        (
            "CHK001:acme/gunicorn_config.py",
            "a string literal names the module, which may be loaded by name: `python:acme.gunicorn_config` in `acme/cli.py:3`",
        ),
    ] {
        let explain = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--no-cache", "--explain", target])
            .arg(project.path())
            .output()
            .expect("run chokkin");
        let stderr = String::from_utf8(explain.stderr).expect("utf8");
        assert!(stderr.contains(reason), "{target}: {stderr}");
    }
}

/// A literal a loader API fetches by name (`import_string("pkg.mod.Cls")`,
/// celery's `symbol_by_name("pkg.mod:Cls")`) imports the module, and the
/// class it names counts as used; a name nothing resolves is no unresolved
/// import, since the loader may be handed a bad name on purpose. A function
/// `module_loaders` names loads its argument the same way; without it the
/// string only lowers the file it names (#493).
#[test]
fn binary_loader_api_strings_import_the_module_they_name() {
    let base = "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[project.scripts]\nacme = \"acme.cli:main\"\n\n[tool.chokkin]\nmode = \"app\"\n";
    let level = |code: &str, target: &str, severity: &str, confidence: &str| {
        (
            code.to_owned(),
            target.to_owned(),
            severity.to_owned(),
            confidence.to_owned(),
        )
    };
    for (pyproject, vision) in [
        (format!("{base}module_loaders = [\"LazyFeature\"]\n"), None),
        (
            base.to_owned(),
            Some(level("CHK001", "acme/features/vision.py", "info", "likely")),
        ),
    ] {
        let project = write_project(&[
            ("pyproject.toml", pyproject.as_str()),
            ("acme/__init__.py", ""),
            (
                "acme/cli.py",
                "from acme.loading import LazyFeature, import_string, symbol_by_name\n\n\ndef main():\n    backend = import_string(\"acme.backends.redis.RedisBackend\")\n    worker = symbol_by_name(\"acme.workers:Worker\")\n    missing = import_string(\"a.aa\")\n    feature = LazyFeature(\"acme.features.vision\")\n    return backend, worker, missing, feature\n",
            ),
            (
                "acme/loading.py",
                "def import_string(path):\n    return path\n\n\ndef symbol_by_name(path):\n    return path\n\n\ndef LazyFeature(path):\n    return path\n",
            ),
            ("acme/backends/__init__.py", ""),
            (
                "acme/backends/redis.py",
                "class RedisBackend:\n    pass\n\n\nclass Unused:\n    pass\n",
            ),
            ("acme/workers.py", "class Worker:\n    pass\n"),
            ("acme/features/__init__.py", ""),
            ("acme/features/vision.py", ""),
            ("acme/orphan.py", ""),
        ]);
        let mut expected: Vec<_> = vision.into_iter().collect();
        expected.push(level("CHK001", "acme/orphan.py", "warning", "certain"));
        expected.push(level(
            "CHK006",
            "acme/backends/redis.py:Unused",
            "warning",
            "likely",
        ));
        assert_eq!(issue_levels(project.path()), expected, "{pyproject}");
    }
}

#[test]
fn binary_root_examples_and_notebooks_count_as_dev_usage() {
    // #1023: a flat layout read neither `examples/` nor `notebooks/`, so the
    // module and the dev dependencies only they use read as unused; reading
    // them must not make the notebook runtime code (CHK005).
    let temp = tempfile::TempDir::new().expect("tempdir");
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = []\n\n[dependency-groups]\ndev = [\"matplotlib\", \"seaborn\"]\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/plotting.py", "def plot():\n    pass\n"),
        (
            "examples/demo.py",
            "import matplotlib.pyplot as plt\nfrom acme.plotting import plot\nplot()\n",
        ),
        (
            "notebooks/demo.ipynb",
            r#"{"cells": [{"cell_type": "code", "source": "import seaborn\n"}]}"#,
        ),
    ] {
        let path = temp.path().join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write fixture");
    }
    let issues = json_issues(temp.path(), &["--strict"]);
    assert_eq!(issue_keys(&issues), [], "{issues:#?}");
}

/// Orchestrators load their code by path or by name from their own config,
/// so nothing imports it (#401): each framework's files and what they import
/// stay unreported, and a config form the plugin does not read leaves the
/// unrelated orphan likely rather than certain (ADR 0005). A case is the
/// pyproject, the framework's files, the CHK001 targets and their confidence.
type OrchestrationCase = (
    String,
    Vec<(&'static str, &'static str)>,
    &'static [&'static str],
    &'static str,
);

/// What a definition the plugin did not read leaves unreached: the helper
/// only it imports, and the orphan, both capped by the gap.
const ORPHAN: &[&str] = &["acme/orphan.py"];
const UNREACHED: &[&str] = &["acme/helper.py", "acme/orphan.py"];
const AIRFLOW_DAG: &str =
    "from airflow import DAG\nfrom acme import helper\n\nwith DAG(\"etl\"):\n    helper.run()\n";

fn orchestration_pyproject(deps: &str, extra: &str) -> String {
    format!(
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [{deps}]\n\n[tool.chokkin]\nmode = \"app\"\nentry = [\"acme/__init__.py\"]\n{extra}"
    )
}

fn assert_orchestration_cases(cases: Vec<OrchestrationCase>) {
    for (pyproject, files, targets, confidence) in cases {
        let mut files = files.clone();
        files.extend([
            ("pyproject.toml", pyproject.as_str()),
            ("acme/__init__.py", ""),
            ("acme/helper.py", "def run():\n    pass\n\nasset = None\n"),
            ("acme/orphan.py", "# orphan\n"),
        ]);
        let project = write_project(&files);
        let issues: Vec<(String, String, String)> = json_issues(project.path(), &["--no-cache"])
            .iter()
            .filter(|issue| issue["code"] == "CHK001")
            .map(|issue| {
                let field = |key: &str| issue[key].as_str().unwrap_or_default().to_owned();
                (field("code"), field("target"), field("confidence"))
            })
            .collect();
        let expected: Vec<(String, String, String)> = targets
            .iter()
            .map(|target| {
                (
                    "CHK001".to_owned(),
                    (*target).to_owned(),
                    confidence.to_owned(),
                )
            })
            .collect();
        assert_eq!(issues, expected, "{files:?}");
    }
}

#[test]
fn binary_airflow_plugin_roots_its_definitions() {
    assert_orchestration_cases(vec![
        // DAG files under the default `dags/` and a test `dags_*` folder.
        (
            orchestration_pyproject("\"apache-airflow\"", ""),
            vec![
                ("dags/etl.py", AIRFLOW_DAG),
                (
                    "tests/dags_corrupted/load.py",
                    "from airflow.sdk import dag\n\n@dag()\ndef load():\n    pass\n",
                ),
            ],
            ORPHAN,
            "certain",
        ),
        // `dags_folder` from `airflow.cfg`, relative to `AIRFLOW_HOME`.
        (
            orchestration_pyproject("\"apache-airflow\"", ""),
            vec![
                (
                    "airflow.cfg",
                    "[core]\ndags_folder = {AIRFLOW_HOME}/pipelines\n",
                ),
                ("pipelines/etl.py", AIRFLOW_DAG),
            ],
            ORPHAN,
            "certain",
        ),
        // An absolute `dags_folder` is not read.
        (
            orchestration_pyproject("\"apache-airflow\"", ""),
            vec![
                ("airflow.cfg", "[core]\ndags_folder = /opt/airflow/flows\n"),
                ("dags/etl.py", AIRFLOW_DAG),
            ],
            ORPHAN,
            "likely",
        ),
    ]);
}

#[test]
fn binary_dagster_plugin_roots_its_definitions() {
    assert_orchestration_cases(vec![
        // Dagster's `[tool.dagster] module_name` and `Definitions(...)`.
        (
            orchestration_pyproject(
                "\"dagster\"",
                "\n[tool.dagster]\nmodule_name = \"acme_dg.definitions\"\n",
            ),
            vec![
                ("acme_dg/__init__.py", ""),
                (
                    "acme_dg/definitions.py",
                    "from dagster import Definitions\nfrom acme import helper\n\ndefs = Definitions(assets=[helper.asset])\n",
                ),
            ],
            ORPHAN,
            "certain",
        ),
        // `workspace.yaml` code locations by file and by module.
        (
            orchestration_pyproject("\"dagster\"", ""),
            vec![
                (
                    "workspace.yaml",
                    "load_from:\n  - python_file:\n      relative_path: jobs/repo.py\n      location_name: jobs\n  - python_module: acme_ops.defs\n",
                ),
                (
                    "jobs/repo.py",
                    "import dagster as dg\nfrom acme import helper\n\n@dg.job\ndef job():\n    helper.run()\n",
                ),
                ("acme_ops/__init__.py", ""),
                ("acme_ops/defs.py", "from acme import helper\n"),
            ],
            ORPHAN,
            "certain",
        ),
        // A flow sequence is not read.
        (
            orchestration_pyproject("\"dagster\"", ""),
            vec![
                (
                    "workspace.yaml",
                    "load_from: [{python_module: acme_ops.defs}]\n",
                ),
                ("acme_ops/__init__.py", ""),
                ("acme_ops/defs.py", "from acme import helper\n"),
            ],
            UNREACHED,
            "likely",
        ),
        // A `load_from` kind that is not read.
        (
            orchestration_pyproject("\"dagster\"", ""),
            vec![
                (
                    "workspace.yaml",
                    "load_from:\n  - python_entrypoint: acme_ops.defs\n",
                ),
                ("acme_ops/__init__.py", ""),
                (
                    "acme_ops/defs.py",
                    "import dagster\nfrom acme import helper\n",
                ),
            ],
            UNREACHED,
            "likely",
        ),
    ]);
}

#[test]
fn binary_prefect_plugin_roots_its_definitions() {
    assert_orchestration_cases(vec![
        // Prefect's `prefect.yaml` entrypoint `path.py:flow`.
        (
            orchestration_pyproject("\"prefect\"", ""),
            vec![
                (
                    "prefect.yaml",
                    "name: acme\ndeployments:\n  - name: etl\n    entrypoint: flows/etl.py:etl_flow\n    work_pool:\n      name: default\n",
                ),
                (
                    "flows/etl.py",
                    "from prefect import flow\nfrom acme import helper\n\n@flow\ndef etl_flow():\n    helper.run()\n",
                ),
            ],
            ORPHAN,
            "certain",
        ),
        // A templated entrypoint is not read.
        (
            orchestration_pyproject("\"prefect\"", ""),
            vec![
                (
                    "prefect.yaml",
                    "deployments:\n  - name: etl\n    entrypoint: \"{{ $FLOW_ENTRYPOINT }}\"\n",
                ),
                (
                    "flows/etl.py",
                    "from prefect import flow\nfrom acme import helper\n",
                ),
            ],
            UNREACHED,
            "likely",
        ),
    ]);
}

#[test]
fn binary_scrapy_plugin_roots_its_definitions() {
    assert_orchestration_cases(vec![
        // Scrapy's settings from `scrapy.cfg`, its spiders and pipelines.
        (
            orchestration_pyproject("\"scrapy\"", ""),
            vec![
                ("scrapy.cfg", "[settings]\ndefault = crawler.settings\n"),
                ("crawler/__init__.py", ""),
                (
                    "crawler/settings.py",
                    "BOT_NAME = \"crawler\"\nSPIDER_MODULES = [\"crawler.spiders\"]\nITEM_PIPELINES = {\"crawler.pipelines.SavePipeline\": 300}\n",
                ),
                ("crawler/spiders/__init__.py", ""),
                (
                    "crawler/spiders/quotes.py",
                    "import scrapy\nfrom acme import helper\n\nclass QuotesSpider(scrapy.Spider):\n    name = \"quotes\"\n",
                ),
                ("crawler/pipelines.py", "class SavePipeline:\n    pass\n"),
            ],
            ORPHAN,
            "certain",
        ),
        // Settings split across modules by a star import.
        (
            orchestration_pyproject("\"scrapy\"", ""),
            vec![
                ("scrapy.cfg", "[settings]\ndefault = crawler.settings\n"),
                ("crawler/__init__.py", ""),
                (
                    "crawler/settings.py",
                    "from .base import *\n\nLOG_LEVEL = \"INFO\"\n",
                ),
                (
                    "crawler/base.py",
                    "SPIDER_MODULES = [\"crawler.spiders\"]\nITEM_PIPELINES = {\"crawler.pipelines.SavePipeline\": 300}\n",
                ),
                ("crawler/spiders/__init__.py", ""),
                (
                    "crawler/spiders/quotes.py",
                    "import scrapy\nfrom acme import helper\n",
                ),
                ("crawler/pipelines.py", "class SavePipeline:\n    pass\n"),
            ],
            ORPHAN,
            "certain",
        ),
        // A namespace package of spiders, walked as `pkgutil` walks it.
        (
            orchestration_pyproject("\"scrapy\"", ""),
            vec![
                ("scrapy.cfg", "[settings]\ndefault = crawler.settings\n"),
                ("crawler/__init__.py", ""),
                (
                    "crawler/settings.py",
                    "SPIDER_MODULES = [\"crawler.spiders\"]\n",
                ),
                (
                    "crawler/spiders/quotes.py",
                    "import scrapy\nfrom acme import helper\n",
                ),
            ],
            ORPHAN,
            "certain",
        ),
        // `SPIDER_MODULES` computed at runtime is not read.
        (
            orchestration_pyproject("\"scrapy\"", ""),
            vec![
                ("scrapy.cfg", "[settings]\ndefault = crawler.settings\n"),
                ("crawler/__init__.py", ""),
                (
                    "crawler/settings.py",
                    "import os\nSPIDER_MODULES = os.environ[\"SPIDERS\"].split(\",\")\n",
                ),
                ("crawler/spiders/__init__.py", ""),
                (
                    "crawler/spiders/quotes.py",
                    "import scrapy\nfrom acme import helper\n",
                ),
            ],
            UNREACHED,
            "likely",
        ),
    ]);
}

/// The orchestrators read their files' globals themselves: the DAG processor
/// collects the `DAG`s a file binds, dagster and scrapy scan a module for
/// definitions and spiders, scrapy reads its settings' names. Those names
/// are no API surface even inside the package (#401). Prefect reads only
/// the entrypoint's function, so the flow file's other names still report,
/// as does an ordinary module beside them.
#[test]
fn binary_orchestration_files_are_no_api_surface() {
    let pyproject = orchestration_pyproject(
        "\"apache-airflow\", \"dagster\", \"prefect\", \"scrapy\"",
        "",
    );
    let project = write_project(&[
        ("pyproject.toml", pyproject.as_str()),
        ("acme/__init__.py", "from acme import lib\n\nlib.used()\n"),
        (
            "acme/lib.py",
            "def used():\n    pass\n\n\ndef unused():\n    pass\n",
        ),
        (
            "acme/example_dags/etl.py",
            "from airflow import DAG\n\nRETRIES = 3\n\nwith DAG(\"etl\") as dag:\n    pass\n",
        ),
        (
            "acme/defs.py",
            "from dagster import Definitions, asset\n\n\n@asset\ndef orders():\n    pass\n\n\ndefs = Definitions(assets=[orders])\n",
        ),
        (
            "prefect.yaml",
            "deployments:\n  - name: etl\n    entrypoint: acme/flows/etl.py:etl_flow\n",
        ),
        (
            "acme/flows/etl.py",
            "from prefect import flow\n\nRETRIES = 3\n\n\n@flow(retries=RETRIES)\ndef etl_flow():\n    pass\n\n\ndef other_flow():\n    pass\n",
        ),
        ("scrapy.cfg", "[settings]\ndefault = acme.settings\n"),
        (
            "acme/settings.py",
            "BOT_NAME = \"acme\"\nSPIDER_MODULES = [\"acme.spiders\"]\n",
        ),
        ("acme/spiders/__init__.py", ""),
        (
            "acme/spiders/quotes.py",
            "import scrapy\n\n\nclass QuotesSpider(scrapy.Spider):\n    name = \"quotes\"\n",
        ),
    ]);
    let issues = json_issues(project.path(), &["--no-cache", "--strict"]);
    let targets: Vec<&str> = issues
        .iter()
        .filter(|issue| matches!(issue["code"].as_str(), Some("CHK001" | "CHK006" | "CHK007")))
        .filter_map(|issue| issue["target"].as_str())
        .collect();
    assert_eq!(
        targets,
        ["acme/flows/etl.py:other_flow", "acme/lib.py:unused"],
        "{issues:#?}"
    );
}

/// A DAG file the walk finds on disk keeps the context its path implies:
/// under `--production` a test DAG is dropped, not a skipped entry (#401).
#[test]
fn binary_airflow_on_disk_test_dag_is_no_production_entry() {
    let pyproject = orchestration_pyproject("\"apache-airflow\"", "");
    let project = write_project(&[
        ("pyproject.toml", pyproject.as_str()),
        ("acme/__init__.py", ""),
        ("dags/etl.py", AIRFLOW_DAG),
        (
            "dags/tests/test_etl.py",
            "from airflow import DAG\n\n\ndef test_dag():\n    DAG(\"t\")\n",
        ),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--no-cache", "--production"])
        .arg(project.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(!stderr.contains("skipped entry"), "{stderr}");
}

/// Poetry, setup.cfg and setup.py declare console scripts as
/// `[project.scripts]` does, and each roots its target (#916).
#[test]
fn binary_entry_points_from_every_manifest_root_their_targets() {
    let manifests = [
        (
            "pyproject.toml",
            "[tool.poetry]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.poetry.scripts]\nacme = \"acme.main:main\"\n",
        ),
        (
            "setup.cfg",
            "[metadata]\nname = acme\n\n[options.entry_points]\nconsole_scripts =\n    acme = acme.main:main\n",
        ),
        (
            "setup.py",
            "from setuptools import setup\n\nsetup(\n    name=\"acme\",\n    entry_points={\"console_scripts\": [\"acme = acme.main:main\"]},\n)\n",
        ),
    ];
    for (manifest, text) in manifests {
        let temp = write_project(&[
            (manifest, text),
            ("acme/__init__.py", ""),
            (
                "acme/main.py",
                "def main():\n    pass\n\n\ndef unused():\n    pass\n",
            ),
        ]);
        let keys = issue_keys(&json_issues(temp.path(), &[]));
        let has = |code: &str, target: &str| keys.contains(&(code.to_owned(), target.to_owned()));
        assert!(!has("CHK006", "acme/main.py:main"), "{manifest}: {keys:?}");
        assert!(!has("CHK001", "acme/main.py"), "{manifest}: {keys:?}");
        assert!(has("CHK006", "acme/main.py:unused"), "{manifest}: {keys:?}");
    }
}

/// A declaration none of the forms describes is skipped with a warning.
#[test]
fn binary_unreadable_entry_point_is_reported() {
    let temp = write_project(&[
        (
            "pyproject.toml",
            "[tool.poetry]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.poetry.scripts]\nacme = 1\n",
        ),
        ("acme/__init__.py", ""),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg(temp.path())
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(
        stderr.contains(
            "manifest: skipped entry point `tool.poetry.scripts.acme` in `pyproject.toml`: not a string or script table"
        ),
        "{stderr}"
    );
}
