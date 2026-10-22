//! Integration tests for optional fix (pipeline step 13).

#![expect(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step; scenarios run end to end"
)]

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

/// #1041: a Poetry group declaration that names where the package comes
/// from (`git`, `path`, `url`, `source`) has no PEP 508 form, so it stays.
#[test]
fn fix_keeps_poetry_sourced_group_dependencies_in_their_group() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pyproject = temp.path().join("pyproject.toml");
    let before = r#"[project]
name = "acme"
version = "0.1.0"
dependencies = []

[tool.poetry.group.dev.dependencies]
gitdep = { git = "https://example.invalid/gitdep.git", rev = "abc" }
pathdep = { path = "../pathdep", develop = true }
urldep = { url = "https://example.invalid/urldep-1.0.tar.gz" }
corpdep = { version = "^1", source = "corp" }
plaindep = "^1.2"
"#;
    std::fs::write(&pyproject, before).expect("write pyproject");
    std::fs::create_dir_all(temp.path().join("acme")).expect("mkdir");
    std::fs::write(temp.path().join("acme/__init__.py"), "").expect("write init");
    std::fs::write(
        temp.path().join("acme/main.py"),
        "import corpdep\nimport gitdep\nimport pathdep\nimport plaindep\nimport urldep\n",
    )
    .expect("write main");

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
    let names = |subjects: Vec<&chokkin::internals::IssueSubject>| {
        let mut names: Vec<String> = subjects
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
        names(
            fix_report
                .applied
                .iter()
                .filter(|fix| fix.rule == RuleId::Chk005)
                .map(|fix| &fix.subject)
                .collect()
        ),
        ["plaindep"]
    );
    let skipped: Vec<_> = fix_report
        .skipped
        .iter()
        .filter(|fix| fix.rule == RuleId::Chk005)
        .collect();
    assert_eq!(
        names(skipped.iter().map(|fix| &fix.subject).collect()),
        ["corpdep", "gitdep", "pathdep", "urldep"]
    );
    assert!(
        skipped
            .iter()
            .all(|fix| fix.reason.to_string() == "ambiguous"),
        "{skipped:?}"
    );

    let after = std::fs::read_to_string(&pyproject).expect("read pyproject after fix");
    let doc: toml::Table = toml::from_str(&after).expect("valid toml");
    let deps = doc["project"]["dependencies"].as_array().expect("array");
    assert_eq!(
        deps.iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>(),
        ["plaindep"],
        "{after}"
    );
    let dev = doc["tool"]["poetry"]["group"]["dev"]["dependencies"]
        .as_table()
        .expect("dev table");
    assert_eq!(
        dev.keys().map(String::as_str).collect::<Vec<_>>(),
        ["corpdep", "gitdep", "pathdep", "urldep"],
        "{after}"
    );
}

/// #730: an import behind an `except ImportError: x = None` guard is not
/// moved to the runtime dependencies.
#[test]
fn fix_keeps_none_fallback_guarded_imports_in_their_group() {
    let temp = tempfile::tempdir().expect("tempdir");
    copy_dir_recursive(&fixture("misplaced_none_fallback"), temp.path()).expect("copy fixture");

    analyze_project(
        temp.path(),
        None,
        &RuntimeOverrides::default(),
        AnalyzeOptions {
            fix_enabled: true,
            ..AnalyzeOptions::default()
        },
    )
    .expect("analyze with fix");

    let after = std::fs::read_to_string(temp.path().join("pyproject.toml")).expect("pyproject");
    let doc: toml::Table = toml::from_str(&after).expect("valid toml");
    let names = |value: &toml::Value| -> Vec<String> {
        let array = value.as_array().expect("array");
        array
            .iter()
            .filter_map(|dep| dep.as_str().map(str::to_owned))
            .collect()
    };
    assert_eq!(
        names(&doc["project"]["dependencies"]),
        ["pyyaml"],
        "{after}"
    );
    assert_eq!(
        names(&doc["dependency-groups"]["dev"]),
        ["markdown", "pygments"],
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
        ["attrs", "cloudpickle", "xarray"]
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
        [
            "dask", "polars", "pyarrow", "pyspark", "sympy", "toolz", "trio"
        ]
    );
}

/// #731: a host's own import in a module it loads is info, so it stays put.
#[test]
fn fix_keeps_host_loaded_imports_out_of_runtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    copy_dir_recursive(&fixture("misplaced_host_loaded"), temp.path()).expect("copy fixture");

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
    assert!(report.fix.is_some());

    let after =
        std::fs::read_to_string(temp.path().join("pyproject.toml")).expect("read pyproject");
    let doc: toml::Table = toml::from_str(&after).expect("valid toml after fix");
    let names = |value: &toml::Value| -> Vec<String> {
        value
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
    };
    assert_eq!(
        names(&doc["project"]["dependencies"]),
        ["sniffio", "rich", "hypothesis"]
    );
    assert_eq!(
        names(&doc["dependency-groups"]["dev"]),
        ["curio", "ipython", "pytest", "trio"]
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

/// #696: CHK009 --fix drops only a repeat in the same context; a group or
/// extra declaration that repeats the runtime one stays as written.
#[test]
fn fix_removes_only_the_same_context_repeat() {
    let temp = tempfile::tempdir().expect("tempdir");
    copy_dir_recursive(&fixture("duplicate_constraints"), temp.path()).expect("copy fixture");
    let pyproject = temp.path().join("pyproject.toml");
    let before = std::fs::read_to_string(&pyproject).expect("read pyproject");

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
    assert_eq!(report.fix.expect("fix report").applied.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&pyproject).expect("read pyproject after fix"),
        before.replace(
            r#"lint = ["rich>=13.0", "anyio", "anyio"]"#,
            r#"lint = ["rich>=13.0", "anyio"]"#
        )
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

fn fix_setup_cfg_project(setup_cfg: &str, source: &str, strict: bool) -> (String, usize) {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    std::fs::create_dir_all(root.join("acme")).expect("mkdir");
    std::fs::write(root.join("acme/__init__.py"), source).expect("init");
    std::fs::write(root.join("setup.cfg"), setup_cfg).expect("setup.cfg");

    let report = analyze_project(
        root,
        None,
        &RuntimeOverrides {
            strict: Some(strict),
            ..RuntimeOverrides::default()
        },
        AnalyzeOptions {
            fix_enabled: true,
            ..AnalyzeOptions::default()
        },
    )
    .expect("analyze with fix");
    let applied = report.fix.expect("fix report").applied.len();
    let after = std::fs::read_to_string(root.join("setup.cfg")).expect("read setup.cfg");
    (after, applied)
}

/// Removing two of four `install_requires` entries rewrites the remaining two
/// one per line; the unindented key after the list ends it and stays.
#[test]
fn fix_rewrites_setup_cfg_install_requires_one_entry_per_line() {
    let (after, applied) = fix_setup_cfg_project(
        "[metadata]\nname = acme\nversion = 0.1\n\n[options]\npackages = find:\ninstall_requires =\n    requests>=2\n    boto3\n    click>=8\n    rich\npython_requires = >=3.9\n",
        "import click\nimport requests\n",
        false,
    );
    assert_eq!(applied, 2);
    assert_eq!(
        after,
        "[metadata]\nname = acme\nversion = 0.1\n\n[options]\npackages = find:\ninstall_requires =\n    requests>=2\n    click>=8\npython_requires = >=3.9\n"
    );
}

/// Removing every `install_requires` entry keeps the emptied key, as an
/// emptied extra keeps its own; an extra (strict) is removed from its own
/// section by line.
#[test]
fn fix_keeps_emptied_setup_cfg_install_requires_and_extra_keys() {
    let (after, applied) = fix_setup_cfg_project(
        "[metadata]\nname = acme\nversion = 0.1\n\n[options]\npackages = find:\ninstall_requires =\n    boto3\n    click>=8\n[options.extras_require]\nyaml =\n    pyyaml\n    requests\n",
        "import requests\n",
        true,
    );
    assert_eq!(applied, 3);
    assert_eq!(
        after,
        "[metadata]\nname = acme\nversion = 0.1\n\n[options]\npackages = find:\ninstall_requires =\n[options.extras_require]\nyaml =\n    requests\n"
    );
}

/// An inline value goes with its entry; the key and every CRLF stay.
#[test]
fn fix_keeps_setup_cfg_crlf_and_inline_keys() {
    let (after, applied) = fix_setup_cfg_project(
        "[metadata]\r\nname = acme\r\n\r\n[options]\r\ninstall_requires = boto3\r\n    click>=8\r\n    rich\r\npython_requires = >=3.9\r\n",
        "import click\n",
        false,
    );
    assert_eq!(applied, 2);
    assert_eq!(
        after,
        "[metadata]\r\nname = acme\r\n\r\n[options]\r\ninstall_requires =\r\n    click>=8\r\npython_requires = >=3.9\r\n"
    );
}

/// Removing an extra's entry touches only that extra: the same name in
/// another extra and a same-named word in an unrelated section stay (one
/// declaration per run; ADR 0003 leaves the next to a fresh analysis).
#[test]
fn fix_removes_setup_cfg_extra_entry_from_its_own_extra_only() {
    let setup_cfg = "[metadata]\nname = acme\n\n[options]\ninstall_requires =\n    click\n\n[options.extras_require]\nyaml =\n    pyyaml\nfull =\n    pyyaml\n    click\n\n[flake8]\nexclude =\n    pyyaml\n";
    let (after, applied) = fix_setup_cfg_project(setup_cfg, "import click\n", true);
    assert_eq!(applied, 1);
    assert_eq!(
        after,
        "[metadata]\nname = acme\n\n[options]\ninstall_requires =\n    click\n\n[options.extras_require]\nyaml =\n    pyyaml\nfull =\n    click\n\n[flake8]\nexclude =\n    pyyaml\n"
    );
}
