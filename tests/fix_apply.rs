//! Integration tests for optional fix (pipeline step 13).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

use std::path::{Path, PathBuf};

use chokkin::internals::{AnalyzeOptions, RuleId};
use chokkin::{ExitStatus, RuntimeOverrides, analyze_project};

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

#[test]
fn fix_moves_direct_reference_with_extras_to_runtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    copy_dir_recursive(&fixture("misplaced_pytest"), temp.path()).expect("copy fixture");
    let pyproject = temp.path().join("pyproject.toml");
    let before = std::fs::read_to_string(&pyproject).expect("read pyproject");
    let url = "https://example.com/pytest-8.0-py3-none-any.whl";
    std::fs::write(
        &pyproject,
        before.replace(
            r#"dev = ["pytest"]"#,
            &format!(r#"dev = ["pytest[testing] @ {url}"]"#),
        ),
    )
    .expect("write pyproject");

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
    let fix_report = report.fix.expect("fix report");
    assert!(
        fix_report
            .applied
            .iter()
            .any(|applied| applied.rule == RuleId::Chk005)
    );

    let after = std::fs::read_to_string(&pyproject).expect("read pyproject after fix");
    let doc: toml::Table = toml::from_str(&after).expect("valid toml");
    let deps = doc["project"]["dependencies"].as_array().expect("array");
    assert!(
        deps.iter()
            .any(|dep| dep.as_str() == Some(&format!("pytest[testing] @ {url}"))),
        "{after}"
    );
}

#[test]
fn fix_moves_only_top_level_misplaced_imports_to_runtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    copy_dir_recursive(&fixture("misplaced_conditional"), temp.path()).expect("copy fixture");

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
    let fix_report = report.fix.expect("fix report");
    let subjects = |rule_fixes: Vec<&chokkin::internals::IssueSubject>| {
        let mut names: Vec<String> = rule_fixes
            .into_iter()
            .filter_map(|subject| match subject {
                chokkin::internals::IssueSubject::Distribution { name } => Some(name.clone()),
                _ => None,
            })
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        subjects(
            fix_report
                .applied
                .iter()
                .filter(|fix| fix.rule == RuleId::Chk005)
                .map(|fix| &fix.subject)
                .collect()
        ),
        ["attrs", "xarray"]
    );
    assert_eq!(
        subjects(
            fix_report
                .skipped
                .iter()
                .filter(|fix| fix.rule == RuleId::Chk005)
                .map(|fix| &fix.subject)
                .collect()
        ),
        ["dask", "polars", "pyarrow", "sympy", "toolz"]
    );
}

#[test]
fn fix_removes_included_group_dependency_only_from_declaring_group() {
    let temp = tempfile::tempdir().expect("tempdir");
    copy_dir_recursive(&fixture("include_group"), temp.path()).expect("copy fixture");

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
    let fix_report = report.fix.expect("fix report");
    assert_eq!(fix_report.applied.len(), 1);
    assert_eq!(fix_report.applied[0].rule, RuleId::Chk002);

    let after =
        std::fs::read_to_string(temp.path().join("pyproject.toml")).expect("read pyproject");
    let doc: toml::Table = toml::from_str(&after).expect("valid toml after fix");
    let groups = doc["dependency-groups"].as_table().expect("groups table");
    assert_eq!(
        groups["Shared_Libs"].as_array().expect("array"),
        &[toml::Value::String("httpx".to_owned())]
    );
    let server = groups["server"].as_array().expect("array");
    assert_eq!(server.len(), 1);
    assert_eq!(
        server[0].get("include-group").and_then(toml::Value::as_str),
        Some("shared-libs")
    );
    assert_eq!(
        groups["dev"].as_array().expect("array")[0]
            .get("include-group")
            .and_then(toml::Value::as_str),
        Some("test")
    );
}

#[test]
fn fix_removes_several_unused_lines_from_one_requirements_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    std::fs::create_dir_all(root.join("src/pkg")).expect("mkdir");
    std::fs::write(root.join("src/pkg/__init__.py"), "").expect("init");
    std::fs::write(
        root.join("src/pkg/main.py"),
        "import bravo\nimport delta\n\ndef main():\n    pass\n",
    )
    .expect("main");
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"pkg\"\nversion = \"0.1\"\n\n[project.scripts]\npkg = \"pkg.main:main\"\n",
    )
    .expect("pyproject");
    let requirements = root.join("requirements.txt");
    std::fs::write(&requirements, "alpha\nbravo\ncharlie\ndelta\n").expect("requirements");

    let report = analyze_project(
        root,
        None,
        &RuntimeOverrides::default(),
        AnalyzeOptions {
            fix_enabled: true,
            ..AnalyzeOptions::default()
        },
    )
    .expect("analyze with fix");

    assert_eq!(report.fix.expect("fix report").applied.len(), 2);
    assert_eq!(
        std::fs::read_to_string(&requirements).expect("read requirements"),
        "bravo\ndelta\n"
    );
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
