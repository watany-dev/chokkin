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
    // Every layout also scans root `docs/`, so `docs/conf.py` is discovered (#1023).
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

/// The reason a manifest cannot be read is printed with its path (#1160).
#[test]
fn binary_probe_non_utf8_pyproject_names_the_reason() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    // UTF-16LE with a BOM, as some Windows editors save it.
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(
            "[project]\nname = \"x\"\n"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        )
        .collect();
    std::fs::write(temp.path().join("pyproject.toml"), utf16).expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--probe")
        .arg(temp.path())
        .output()
        .expect("run chokkin");
    assert_eq!(
        output.status.code(),
        Some(ExitStatus::UsageError.code().into())
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("pyproject.toml: "), "{stderr}");
    assert!(stderr.contains("UTF-8"), "{stderr}");
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

/// The hand-written `USAGE` in `main.rs` must not drift from the clap
/// definition: every long flag is listed, flags that clap rejects without a
/// companion say so, and descriptions share one column (#1166).
#[test]
fn binary_help_matches_clap_definition() {
    use clap::CommandFactory;
    use clap::error::ErrorKind;

    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--help")
        .output()
        .expect("run chokkin");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let option_lines: Vec<&str> = stdout
        .lines()
        .skip_while(|line| *line != "Options:")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .collect();
    let description_column = |line: &str| {
        let flag_end = line.trim_start().find("  ").expect("description gap");
        let indent = line.len() - line.trim_start().len();
        let rest = &line[indent + flag_end..];
        indent + flag_end + (rest.len() - rest.trim_start().len())
    };
    let column = description_column(option_lines[0]);
    for line in &option_lines {
        let actual = if line.trim_start().starts_with('-') {
            description_column(line)
        } else {
            line.len() - line.trim_start().len()
        };
        assert_eq!(actual, column, "misaligned help line: {line:?}");
    }

    let mut command = chokkin::CliArgs::command();
    command.build();
    for arg in command.get_arguments() {
        let Some(long) = arg.get_long() else { continue };
        let flag = format!("--{long}");
        let line = option_lines
            .iter()
            .find(|line| {
                line.split_whitespace()
                    .any(|word| word.trim_end_matches(',') == flag)
            })
            .unwrap_or_else(|| panic!("{flag} missing from --help"));
        if arg.get_action().takes_values() {
            continue;
        }
        let missing_companion = chokkin::CliArgs::command()
            .try_get_matches_from(["chokkin", flag.as_str()])
            .is_err_and(|err| err.kind() == ErrorKind::MissingRequiredArgument);
        assert_eq!(
            line.contains("(requires --"),
            missing_companion,
            "--help line for {flag} disagrees with clap `requires`: {line:?}"
        );
    }
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
