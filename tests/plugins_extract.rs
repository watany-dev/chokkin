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
    discover_sources, extract_manifest, extract_plugin_hints, load_config, parse_project_sources,
    resolve_target_version,
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
    extract_at(&fixture(name))
}

fn extract_at(path: &Path) -> chokkin::PluginHints {
    let root = discover_project_root(path).unwrap_or_else(|_| project_root_at(path));
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    let sources = discover_sources(&root, &config, &manifest).expect("discover sources");
    let target = resolve_target_version(&config.effective, &manifest);
    let parse = parse_project_sources(&root, &sources, &target).expect("parse sources");
    extract_plugin_hints(&root, &config, &sources, &manifest, &parse).expect("extract plugin hints")
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

fn plugin_contrib(hints: &chokkin::PluginHints, plugin: PluginId) -> &chokkin::PluginContribution {
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
    assert!(modules.contains(&"django.middleware.security.SecurityMiddleware"));
    assert!(modules.contains(&"mysite.urls"));
    assert!(contrib.symbol_refs.iter().any(|reference| {
        reference.module == "mysite.wsgi" && reference.symbol == "application"
    }));
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
fn fastapi_src_main_is_plugin_entry() {
    let hints = extract_fixture("fastapi_src_main");
    assert_eq!(entry_paths(fastapi_contrib(&hints)), vec!["src/main.py"]);
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
    assert!(contrib.binary_usages.iter().any(|usage| {
        usage.binary == "pre-commit" && usage.origin.file == ".pre-commit-config.yaml"
    }));
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
                && usage.origin.line == Some(13))
    );
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "uv"
                && usage.origin.file == ".github/workflows/ci.yml"
                && usage.origin.line == Some(15))
    );
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "pytest"
                && usage.origin.file == ".github/workflows/ci.yml"
                && usage.origin.line == Some(15))
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
    assert!(
        contrib
            .module_refs
            .iter()
            .any(|reference| reference.module == "web.routes"
                && reference.origin.file == "src/web/routes.py"
                && reference.origin.line == Some(4))
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
    assert!(
        contrib
            .module_refs
            .iter()
            .any(|reference| reference.module == "worker.tasks"
                && reference.origin.file == "src/worker/tasks.py"
                && reference.origin.line == Some(4))
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
    assert!(
        contrib
            .module_refs
            .iter()
            .any(|reference| reference.module == "sphinx.ext.autodoc"
                && reference.origin.file == "docs/conf.py")
    );
    assert!(
        contrib
            .module_refs
            .iter()
            .any(|reference| reference.module == "myst_parser"
                && reference.origin.file == "docs/conf.py")
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
    assert!(
        hints
            .config_used_distributions
            .contains(&"mkdocs-material".to_owned())
    );
    assert!(
        hints
            .config_used_distributions
            .contains(&"mkdocstrings".to_owned())
    );
    assert!(
        hints
            .config_used_distributions
            .contains(&"mkdocs-autorefs".to_owned())
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
    extract_at(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/deps")
            .join(name),
    )
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

/// Flask and Celery module-ref lines for `pkg.mod` in a one-module project.
fn decorator_lines(source: &str) -> [Option<u32>; 2] {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"decorators\"\nversion = \"0.1.0\"\ndependencies = [\"flask\", \"celery\"]\n\n[tool.chokkin.plugins]\nflask = true\ncelery = true\n",
    )
    .expect("write pyproject");
    std::fs::create_dir_all(root.join("src/pkg")).expect("create package");
    std::fs::write(root.join("src/pkg/__init__.py"), "").expect("write init");
    std::fs::write(root.join("src/pkg/mod.py"), source).expect("write module");

    let hints = extract_at(root);
    [PluginId::Flask, PluginId::Celery].map(|plugin| {
        plugin_contrib(&hints, plugin)
            .module_refs
            .iter()
            .find(|reference| reference.module == "pkg.mod")
            .and_then(|reference| reference.origin.line)
    })
}

#[test]
fn flask_and_celery_decorator_lines() {
    // (source, Flask route line, Celery task line)
    let cases: &[(&str, Option<u32>, Option<u32>)] = &[
        ("@app.route(\"/\")\ndef f():\n    pass\n", Some(1), None),
        ("@app.route (\"/\")\ndef f():\n    pass\n", Some(1), None),
        (
            "@app.route(\n    \"/\",\n)\ndef f():\n    pass\n",
            Some(1),
            None,
        ),
        ("@apps[0].route(\"/\")\ndef f():\n    pass\n", Some(1), None),
        ("@app.route\ndef f():\n    pass\n", None, None),
        ("@cache.get\ndef f():\n    pass\n", None, None),
        (
            "@cache.get\ndef a():\n    pass\n\n@bp.post(\"/x\")\ndef b():\n    pass\n",
            Some(5),
            None,
        ),
        (
            "def create_app():\n    @app.route(\"/\")\n    def index():\n        pass\n",
            Some(2),
            None,
        ),
        (
            "@functools.lru_cache(maxsize=cfg.get(\"n\"))\ndef f():\n    pass\n",
            None,
            None,
        ),
        (
            "@api.post(\"/\") if flag else f\ndef f():\n    pass\n",
            None,
            None,
        ),
        ("@shared_task\ndef f():\n    pass\n", None, Some(1)),
        ("@celery.task\ndef f():\n    pass\n", None, Some(1)),
        ("@my_app.task\ndef f():\n    pass\n", None, Some(1)),
        (
            "@celery.shared_task(bind=True)\ndef f():\n    pass\n",
            None,
            Some(1),
        ),
        ("@shared_task_wrapper\ndef f():\n    pass\n", None, None),
        ("@app.tasks\ndef f():\n    pass\n", None, None),
        ("@app.task_cls\ndef f():\n    pass\n", None, None),
        ("@register(app.task(x))\ndef f():\n    pass\n", None, None),
    ];
    for &(source, flask, celery) in cases {
        let [flask_line, celery_line] = decorator_lines(source);
        assert_eq!(flask_line, flask, "flask: {source}");
        assert_eq!(celery_line, celery, "celery: {source}");
    }
}

#[test]
fn flask_route_modules_survive_syntax_error_in_parse_path() {
    let hints = extract_fixture("flask_syntax_error");
    assert!(
        plugin_contrib(&hints, PluginId::Flask)
            .module_refs
            .iter()
            .any(|reference| reference.module == "web.routes" && reference.origin.line == Some(1))
    );
}

#[test]
fn celery_task_modules_survive_syntax_error_in_parse_path() {
    let hints = extract_fixture("celery_syntax_error");
    assert!(
        plugin_contrib(&hints, PluginId::Celery)
            .module_refs
            .iter()
            .any(|reference| reference.module == "worker.tasks"
                && reference.origin.line == Some(1))
    );
}
