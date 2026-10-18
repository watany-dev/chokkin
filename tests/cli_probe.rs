//! Integration tests for CLI probe mode.

use std::path::PathBuf;
use std::process::Command;

use chokkin::internals::{PluginId, ProjectLayout};
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
    assert_eq!(report.sources.layout.layout, ProjectLayout::Src);
    assert_eq!(report.sources.layout.packages, ["acme"]);
    let files: Vec<_> = report
        .sources
        .python_files()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        files,
        [
            "docs/conf.py",
            "scripts/run.py",
            "src/acme/__init__.py",
            "src/acme/module.py",
            "tests/conftest.py",
            "tests/test_module.py",
        ]
    );
}

#[test]
fn probe_member_notebooks_use_the_members_package_context() {
    use chokkin::internals::FileContext;

    let temp = tempfile::tempdir().expect("tempdir");
    for (path, content) in [
        (
            "pyproject.toml",
            "[project]\nname = 'acme'\nversion = '0.1.0'\n\n[tool.uv.workspace]\nmembers = ['packages/worker']\n",
        ),
        (
            "packages/worker/pyproject.toml",
            "[project]\nname = 'worker'\nversion = '0.1.0'\n",
        ),
        ("packages/worker/src/worker/__init__.py", ""),
        ("packages/worker/src/worker/demo.ipynb", "{}"),
        ("packages/worker/demo.ipynb", "{}"),
        ("packages/worker/notebooks/deep/demo.ipynb", "{}"),
        ("packages/worker/docs/demo.py", ""),
        ("packages/worker/examples/demo.py", ""),
    ] {
        let path = temp.path().join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, content).expect("write");
    }
    let report = probe_project(temp.path(), None, &RuntimeOverrides::default()).expect("probe");
    for (path, context) in [
        ("src/worker/demo.ipynb", FileContext::Runtime),
        ("demo.ipynb", FileContext::Dev),
        ("notebooks/deep/demo.ipynb", FileContext::Dev),
        ("docs/demo.py", FileContext::Docs),
        ("examples/demo.py", FileContext::Dev),
    ] {
        let path = format!("packages/worker/{path}");
        let file = report
            .sources
            .files
            .iter()
            .find(|file| file.path == path)
            .expect(&path);
        assert_eq!(file.context, context, "{path}");
    }
    let production = probe_project(
        temp.path(),
        None,
        &RuntimeOverrides {
            production: Some(true),
            ..RuntimeOverrides::default()
        },
    )
    .expect("production probe");
    let notebooks: Vec<_> = production
        .sources
        .files
        .iter()
        .filter(|file| {
            std::path::Path::new(&file.path)
                .extension()
                .is_some_and(|ext| ext == "ipynb")
        })
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(notebooks, ["packages/worker/src/worker/demo.ipynb"]);
}

#[test]
fn binary_probe_broken_pyproject_exits_two() {
    let root = fixture_path("manifest/broken_pyproject");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--probe")
        .arg(&root)
        .output()
        .expect("run chokkin");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::UsageError.code().into())
    );
    assert_eq!(output.stdout, b"");
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("invalid TOML"), "{stderr}");
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
    assert_eq!(report.workspace_inputs.len(), 2);
    assert!(report.workspace_inputs.iter().any(|input| {
        input.member.id == "api" && input.manifest.metadata.name.as_deref() == Some("api")
    }));
    let mut output = Vec::new();
    write_probe_report(&report, &mut output).expect("write");
    let text = String::from_utf8(output).expect("utf8");
    assert!(text.contains("Workspace: 2 members (2 inventoried)"));
}

#[test]
fn probe_reports_plugin_activation_reasons() {
    let root = fixture_path("probe/plugin_enablers");
    let report = probe_project(&root, None, &RuntimeOverrides::default()).expect("probe");
    let plugins = &report.effective_config.plugins;
    assert_eq!(plugins.get(&PluginId::Flask), Some(&true));
    assert_eq!(plugins.get(&PluginId::MkDocs), Some(&true));
    assert_eq!(plugins.get(&PluginId::Alembic), Some(&false));
    assert_eq!(plugins.get(&PluginId::Celery), Some(&false));

    let mut output = Vec::new();
    write_probe_report(&report, &mut output).expect("write");
    let text = String::from_utf8(output).expect("utf8");
    let expected = "\
Plugins
  pytest           : default
  django           : default
  fastapi          : default
  flask            : enabled-by: dependency flask
  mkdocs           : enabled-by: file mkdocs.yml
  alembic          : disabled-by: config

";
    assert!(text.contains(expected), "{text}");
}

#[test]
fn probe_warnings_written_to_stderr_format() {
    let root = fixture_path("sources/missing_entry");
    let report = probe_project(&root, None, &RuntimeOverrides::default()).expect("probe");
    assert_ne!(report.warnings, []);
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

#[test]
fn probe_falls_back_from_a_stub_uv_lock_and_names_it() {
    // textual's shape (#976): a package-less `uv.lock` beside a real
    // `poetry.lock`. The poetry graph is read, the stub is named both as a
    // warning and on the probe's lockfile line.
    let root = fixture_path("manifest/stub_uv_lock_beside_poetry");
    let report = probe_project(&root, None, &RuntimeOverrides::default()).expect("probe");
    let lockfile = report.manifest.sources.lockfile.as_ref().expect("a lock");
    assert_eq!(lockfile.path, "poetry.lock");
    assert!(report.manifest.lockfile.edges.contains_key("rich"));

    let mut output = Vec::new();
    write_probe_report(&report, &mut output).expect("write");
    let text = String::from_utf8(output).expect("utf8");
    assert!(
        text.contains("lockfile         : poetry.lock (poetry, 2 nodes; not used: uv.lock)"),
        "{text}"
    );
    let mut warnings = Vec::new();
    write_probe_warnings(&report.warnings, &mut warnings).expect("write");
    let warnings = String::from_utf8(warnings).expect("utf8");
    assert!(
        warnings.contains("uv.lock") && warnings.contains("using `poetry.lock` instead"),
        "{warnings}"
    );
}
