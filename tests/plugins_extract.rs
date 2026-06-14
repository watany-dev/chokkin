//! Integration tests for plugin hint extraction (pipeline step 5).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use chokkin::{
    FileContext, PluginId, PluginsWarning, ProjectRoot, RootMarker, discover_project_root,
    discover_sources, extract_manifest, extract_plugin_hints, load_config,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plugins")
        .join(name)
}

fn project_root_at(path: &Path) -> ProjectRoot {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    ProjectRoot {
        path: canonical,
        marker: RootMarker::PyProjectToml,
        start: path.to_path_buf(),
    }
}

fn extract_fixture(name: &str) -> chokkin::PluginHints {
    let path = fixture(name);
    let root = discover_project_root(&path).unwrap_or_else(|_| project_root_at(&path));
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    let sources = discover_sources(&root, &config, &manifest).expect("discover sources");
    extract_plugin_hints(&root, &config, &sources, &manifest).expect("extract plugin hints")
}

fn pytest_contrib(hints: &chokkin::PluginHints) -> &chokkin::PluginContribution {
    hints
        .contributions
        .iter()
        .find(|contrib| contrib.plugin == PluginId::Pytest)
        .expect("pytest contribution")
}

fn django_contrib(hints: &chokkin::PluginHints) -> &chokkin::PluginContribution {
    hints
        .contributions
        .iter()
        .find(|contrib| contrib.plugin == PluginId::Django)
        .expect("django contribution")
}

fn fastapi_contrib(hints: &chokkin::PluginHints) -> &chokkin::PluginContribution {
    hints
        .contributions
        .iter()
        .find(|contrib| contrib.plugin == PluginId::Fastapi)
        .expect("fastapi contribution")
}

fn plugin_contrib(
    hints: &chokkin::PluginHints,
    plugin: PluginId,
) -> &chokkin::PluginContribution {
    hints
        .contributions
        .iter()
        .find(|contrib| contrib.plugin == plugin)
        .expect("plugin contribution")
}

fn entry_paths(contrib: &chokkin::PluginContribution) -> Vec<&str> {
    contrib
        .entries
        .iter()
        .map(|entry| entry.spec.path.as_str())
        .collect()
}

#[test]
fn pytest_discovers_test_files() {
    let hints = extract_fixture("pytest_pyproject");
    let contrib = pytest_contrib(&hints);
    let paths = entry_paths(contrib);
    assert!(paths.contains(&"tests/test_sample.py"));
    assert!(
        contrib
            .entries
            .iter()
            .all(|entry| entry.context == FileContext::Test)
    );
}

#[test]
fn pytest_respects_testpaths() {
    let hints = extract_fixture("pytest_pyproject");
    let contrib = pytest_contrib(&hints);
    let paths = entry_paths(contrib);
    assert!(paths.contains(&"tests/test_sample.py"));
    assert!(!paths.iter().any(|path| path.starts_with("src/")));
}

#[test]
fn pytest_conftest_entry() {
    let hints = extract_fixture("pytest_pyproject");
    let contrib = pytest_contrib(&hints);
    let paths = entry_paths(contrib);
    assert!(paths.contains(&"tests/conftest.py"));
}

#[test]
fn pytest_binary_usage() {
    let hints = extract_fixture("pytest_pyproject");
    let contrib = pytest_contrib(&hints);
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "pytest")
    );
}

#[test]
fn pytest_ini_fixture() {
    let hints = extract_fixture("pytest_ini");
    let contrib = pytest_contrib(&hints);
    assert!(entry_paths(contrib).contains(&"tests/test_one.py"));
}

#[test]
fn pytest_setup_cfg_fixture() {
    let hints = extract_fixture("pytest_setup_cfg");
    let contrib = pytest_contrib(&hints);
    assert!(entry_paths(contrib).contains(&"tests/test_cfg.py"));
}

#[test]
fn django_installed_apps() {
    let hints = extract_fixture("django_manage");
    let contrib = django_contrib(&hints);
    let modules: Vec<&str> = contrib
        .module_refs
        .iter()
        .map(|reference| reference.module.as_str())
        .collect();
    assert!(modules.contains(&"django.contrib.admin"));
    assert!(modules.contains(&"myapp"));
    assert!(modules.contains(&"mysite.urls"));
}

#[test]
fn django_migrations_framework_used() {
    let hints = extract_fixture("django_migrations");
    let contrib = django_contrib(&hints);
    assert!(
        contrib
            .framework_used_globs
            .iter()
            .any(|glob| glob.pattern == "**/migrations/**/*.py")
    );
}

#[test]
fn django_manage_entry() {
    let hints = extract_fixture("django_manage");
    let contrib = django_contrib(&hints);
    let paths = entry_paths(contrib);
    assert!(paths.contains(&"manage.py"));
    assert!(paths.contains(&"mysite/settings.py"));
    assert!(paths.contains(&"mysite/urls.py"));
}

#[test]
fn fastapi_uvicorn_app_symbol() {
    let hints = extract_fixture("fastapi_uvicorn_tool");
    let contrib = fastapi_contrib(&hints);
    assert!(
        contrib
            .symbol_refs
            .iter()
            .any(|reference| { reference.module == "pkg.main" && reference.symbol == "app" })
    );
}

#[test]
fn fastapi_scripts_symbol() {
    let hints = extract_fixture("fastapi_scripts");
    let contrib = fastapi_contrib(&hints);
    assert!(
        contrib
            .symbol_refs
            .iter()
            .any(|reference| { reference.module == "pkg.main" && reference.symbol == "app" })
    );
}

#[test]
fn disabled_plugin_skipped() {
    let hints = extract_fixture("plugins_disabled");
    assert!(hints.contributions.is_empty());
}

#[test]
fn tox_plugin_records_config_binary() {
    let hints = extract_fixture("tox_config");
    let contrib = plugin_contrib(&hints, PluginId::Tox);
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "tox" && usage.origin.file == "tox.ini")
    );
    assert!(!hints.warnings.iter().any(|warning| {
        matches!(
            warning,
            PluginsWarning::PluginNoOp {
                plugin: PluginId::Tox
            }
        )
    }));
}

#[test]
fn nox_plugin_records_config_binary() {
    let hints = extract_fixture("nox_config");
    let contrib = plugin_contrib(&hints, PluginId::Nox);
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "nox" && usage.origin.file == "noxfile.py")
    );
}

#[test]
fn pre_commit_plugin_records_config_binary() {
    let hints = extract_fixture("pre_commit_config");
    let contrib = plugin_contrib(&hints, PluginId::PreCommit);
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| {
                usage.binary == "pre-commit" && usage.origin.file == ".pre-commit-config.yaml"
            })
    );
}

#[test]
fn github_actions_plugin_records_run_binaries() {
    let hints = extract_fixture("github_actions_workflow");
    let contrib = plugin_contrib(&hints, PluginId::GithubActions);
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "ruff"
                && usage.origin.file == ".github/workflows/ci.yml"
                && usage.origin.line == Some(11))
    );
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "mypy"
                && usage.origin.file == ".github/workflows/ci.yml"
                && usage.origin.line == Some(12))
    );
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "pytest"
                && usage.origin.file == ".github/workflows/ci.yml"
                && usage.origin.line == Some(14))
    );
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "uv"
                && usage.origin.file == ".github/workflows/ci.yml"
                && usage.origin.line == Some(14))
    );
}

#[test]
fn flask_plugin_records_flask_app_symbol() {
    let hints = extract_fixture("flask_env");
    let contrib = plugin_contrib(&hints, PluginId::Flask);
    assert!(
        contrib
            .symbol_refs
            .iter()
            .any(|reference| reference.module == "web.app" && reference.symbol == "app")
    );
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "flask" && usage.origin.file == ".flaskenv")
    );
}

#[test]
fn celery_plugin_records_app_symbol() {
    let hints = extract_fixture("celery_scripts");
    let contrib = plugin_contrib(&hints, PluginId::Celery);
    assert!(
        contrib
            .symbol_refs
            .iter()
            .any(|reference| reference.module == "worker.app" && reference.symbol == "celery")
    );
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "celery" && usage.origin.file == "pyproject.toml")
    );
}

#[test]
fn sphinx_plugin_records_docs_conf_entry() {
    let hints = extract_fixture("sphinx_docs");
    let contrib = plugin_contrib(&hints, PluginId::Sphinx);
    let paths = entry_paths(contrib);
    assert!(paths.contains(&"docs/conf.py"));
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "sphinx-build" && usage.origin.file == "docs/conf.py")
    );
}

#[test]
fn mkdocs_plugin_records_config_binary() {
    let hints = extract_fixture("mkdocs_config");
    let contrib = plugin_contrib(&hints, PluginId::MkDocs);
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "mkdocs" && usage.origin.file == "mkdocs.yml")
    );
}

#[test]
fn alembic_plugin_records_env_entry() {
    let hints = extract_fixture("alembic_env");
    let contrib = plugin_contrib(&hints, PluginId::Alembic);
    let paths = entry_paths(contrib);
    assert!(paths.contains(&"alembic/env.py"));
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "alembic" && usage.origin.file == "alembic.ini")
    );
}

#[test]
fn full_pipeline_step5() {
    let hints = extract_fixture("django_manage");
    assert_eq!(hints.contributions.len(), 3);
    assert!(
        pytest_contrib(&hints)
            .entries
            .iter()
            .any(|entry| { entry.spec.path.contains("test") })
            || hints.warnings.iter().any(|warning| {
                matches!(
                    warning,
                    PluginsWarning::PluginNoOp {
                        plugin: PluginId::Pytest
                    }
                )
            })
    );
}

#[test]
fn partial_settings_warns() {
    let hints = extract_fixture("django_partial_settings");
    assert!(
        hints
            .warnings
            .iter()
            .any(|warning| { matches!(warning, PluginsWarning::PartialSettingsParse { .. }) })
    );
}

fn extract_fixture_from_deps(name: &str) -> chokkin::PluginHints {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deps")
        .join(name);
    let root = discover_project_root(&path).unwrap_or_else(|_| project_root_at(&path));
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    let sources = discover_sources(&root, &config, &manifest).expect("discover sources");
    extract_plugin_hints(&root, &config, &sources, &manifest).expect("extract plugin hints")
}

#[test]
fn config_scan_deps_fixture() {
    let hints = extract_fixture_from_deps("binary_tool_pyproject");
    assert!(
        hints
            .config_binary_usages
            .iter()
            .any(|usage| usage.binary == "mypy")
    );
    assert!(
        hints
            .config_binary_usages
            .iter()
            .any(|usage| usage.binary == "ruff")
    );
}

#[test]
fn no_django_no_panic() {
    let hints = extract_fixture("no_django");
    let django = django_contrib(&hints);
    assert!(django.entries.is_empty());
    assert!(hints.warnings.iter().any(|warning| {
        matches!(
            warning,
            PluginsWarning::PluginNoOp {
                plugin: PluginId::Django
            }
        )
    }));
}
