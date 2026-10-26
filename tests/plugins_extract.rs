//! Integration tests for plugin hint extraction (pipeline step 5).

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::path::{Path, PathBuf};

use chokkin::internals::{
    FileContext, PluginExtractRequest, PluginId, PluginsWarning, ProjectRoot, RootMarker,
    discover_project_root, discover_sources, extract_manifest, extract_plugin_hints_with_parse,
    load_config, parse_project_sources_with_cache, resolve_target_version,
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
    }
}

fn extract_fixture(name: &str) -> chokkin::internals::PluginHints {
    extract_at(&fixture(name))
}

fn extract_at(path: &Path) -> chokkin::internals::PluginHints {
    let root = discover_project_root(path).unwrap_or_else(|_| project_root_at(path));
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");
    let sources = discover_sources(&root, &config, &manifest).expect("discover sources");
    let target = resolve_target_version(&config.effective, &manifest);
    let parse =
        parse_project_sources_with_cache(&root, &sources, &target, None).expect("parse sources");
    extract_plugin_hints_with_parse(&PluginExtractRequest {
        root: &root,
        config: &config,
        sources: &sources,
        manifest: &manifest,
        parse: &parse,
        cache: None,
        dockerfiles: &[],
    })
    .expect("extract plugin hints")
}

fn pytest_contrib(
    hints: &chokkin::internals::PluginHints,
) -> &chokkin::internals::PluginContribution {
    hints
        .contributions
        .iter()
        .find(|contrib| contrib.plugin == PluginId::Pytest)
        .expect("pytest contribution")
}

fn django_contrib(
    hints: &chokkin::internals::PluginHints,
) -> &chokkin::internals::PluginContribution {
    hints
        .contributions
        .iter()
        .find(|contrib| contrib.plugin == PluginId::Django)
        .expect("django contribution")
}

fn fastapi_contrib(
    hints: &chokkin::internals::PluginHints,
) -> &chokkin::internals::PluginContribution {
    hints
        .contributions
        .iter()
        .find(|contrib| contrib.plugin == PluginId::Fastapi)
        .expect("fastapi contribution")
}

fn plugin_contrib(
    hints: &chokkin::internals::PluginHints,
    plugin: PluginId,
) -> &chokkin::internals::PluginContribution {
    hints
        .contributions
        .iter()
        .find(|contrib| contrib.plugin == plugin)
        .expect("plugin contribution")
}

fn entry_paths(contrib: &chokkin::internals::PluginContribution) -> Vec<&str> {
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
    // A non-default `testpaths` so dropping it (falling back to `tests`) or
    // ignoring it (matching `test_*.py` everywhere) both change the result.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.pytest.ini_options]\ntestpaths = [\"tests/unit\"]\n",
    )
    .expect("write pyproject");
    for rel in [
        "src/acme/__init__.py",
        "src/acme/test_in_src.py",
        "tests/test_top.py",
        "tests/unit/test_unit.py",
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        std::fs::write(path, "").expect("write file");
    }

    let hints = extract_at(root);
    let paths = entry_paths(pytest_contrib(&hints));
    assert!(paths.contains(&"tests/unit/test_unit.py"));
    assert!(!paths.contains(&"tests/test_top.py"));
    assert!(!paths.contains(&"src/acme/test_in_src.py"));
}

#[test]
fn pytest_searches_rootdir_when_no_testpaths_exist() {
    // airflow sets `testpaths = ["tests"]` without a root `tests/`; pytest then
    // collects from the rootdir instead.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.pytest]\ntestpaths = [\"tests\"]\n",
    )
    .expect("write pyproject");
    for rel in ["acme/__init__.py", "acme/test_in_pkg.py"] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        std::fs::write(path, "").expect("write file");
    }

    let hints = extract_at(root);
    let paths = entry_paths(pytest_contrib(&hints));
    assert!(paths.contains(&"acme/test_in_pkg.py"));
}

#[test]
fn pytest_reads_native_tool_pytest_table() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\n\n[tool.pytest]\npython_files = [\"example_*.py\"]\n",
    )
    .expect("write pyproject");
    for rel in ["acme/__init__.py", "tests/system/example_x.py"] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        std::fs::write(path, "").expect("write file");
    }

    let hints = extract_at(root);
    let contrib = pytest_contrib(&hints);
    let entry = contrib
        .entries
        .iter()
        .find(|entry| entry.spec.path == "tests/system/example_x.py")
        .expect("example_x.py entry");
    assert_eq!(entry.context, FileContext::Test);
    assert_eq!(entry.origin.label, "tool.pytest");
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

/// Each module a settings string names carries that string's line (#830).
#[test]
fn django_module_refs_carry_their_string_line() {
    let hints = extract_fixture("django_manage");
    let contrib = django_contrib(&hints);
    let lines: Vec<(&str, &str, Option<u32>)> = contrib
        .module_refs
        .iter()
        .map(|reference| {
            (
                reference.module.as_str(),
                reference.origin.file.as_str(),
                reference.origin.line,
            )
        })
        .collect();
    for expected in [
        ("mysite.settings", "manage.py", Some(6)),
        ("django.contrib.admin", "mysite/settings.py", Some(2)),
        ("myapp", "mysite/settings.py", Some(4)),
        (
            "django.middleware.security.SecurityMiddleware",
            "mysite/settings.py",
            Some(8),
        ),
        ("mysite.urls", "mysite/settings.py", Some(11)),
    ] {
        assert!(lines.contains(&expected), "{expected:?} in {lines:?}");
    }
    assert!(contrib.symbol_refs.iter().any(|reference| {
        reference.module == "mysite.wsgi" && reference.origin.line == Some(12)
    }));
}

/// Plugin hints for a project depending on Django with `files` written.
fn django_project(files: &[(&str, &str)]) -> chokkin::internals::PluginHints {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"site\"\nversion = \"0\"\ndependencies = [\"django\"]\n",
    )
    .expect("write pyproject");
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        std::fs::write(path, text).expect("write file");
    }
    extract_at(root)
}

#[test]
fn django_import_settings_read_known_nested_fields_and_origins() {
    let settings = concat!(
        "INSTALLED_APPS = []\n",
        "AUTHENTICATION_BACKENDS = ['external.auth.Backend']\n",
        "PASSWORD_HASHERS = ('external.hashers.Hasher',)\n",
        "DATABASE_ROUTERS = ['external.router.Router']\n",
        "DEFAULT_FILE_STORAGE = 'external.storage.Storage'\n",
        "STATICFILES_STORAGE = 'external.static.Storage'\n",
        "STORAGES = {'default': {'BACKEND': 'external.media.Storage', 'OPTIONS': {'bucket': 'external.ignore.Value'}}}\n",
        "TEMPLATES = [{'BACKEND': 'external.templates.Backend', 'OPTIONS': {'context_processors': ['external.context.user'], 'libraries': {'tags': 'external.tags'}, 'builtins': ['external.builtins']}}]\n",
        "LOGGING = {'handlers': {'mail': {'class': 'external.log.Handler'}}, 'filters': {'custom': {'()': 'external.log.Filter'}}}\n",
        "AUTH_PASSWORD_VALIDATORS = [{'NAME': 'external.validation.Validator'}]\n",
        "EMAIL_BACKEND = 'external.email.Backend'\n",
        "SESSION_ENGINE = 'external.sessions'\n",
        "TEST_RUNNER = 'external.tests.Runner'\n",
        "FORM_RENDERER = 'external.forms.Renderer'\n",
        "CSRF_FAILURE_VIEW = 'external.csrf.failure'\n",
        "DATABASES = {'default': {'ENGINE': 'external.db', 'NAME': 'external.ignore.Value'}}\n",
        "CACHES = {'default': {'BACKEND': 'external.cache.Backend', 'LOCATION': 'external.ignore.Value'}}\n",
    );
    let hints = django_project(&[("mysite/settings.py", settings)]);
    let refs: Vec<_> = django_contrib(&hints)
        .module_refs
        .iter()
        .map(|reference| (reference.module.as_str(), reference.origin.line))
        .collect();
    for (module, line) in [
        ("external.auth.Backend", 2),
        ("external.hashers.Hasher", 3),
        ("external.router.Router", 4),
        ("external.storage.Storage", 5),
        ("external.static.Storage", 6),
        ("external.media.Storage", 7),
        ("external.templates.Backend", 8),
        ("external.context.user", 8),
        ("external.tags", 8),
        ("external.builtins", 8),
        ("external.log.Handler", 9),
        ("external.log.Filter", 9),
        ("external.validation.Validator", 10),
        ("external.email.Backend", 11),
        ("external.sessions", 12),
        ("external.tests.Runner", 13),
        ("external.forms.Renderer", 14),
        ("external.csrf.failure", 15),
        ("external.db", 16),
        ("external.cache.Backend", 17),
    ] {
        assert!(
            refs.contains(&(module, Some(line))),
            "missing {module}: {refs:?}"
        );
    }
    assert!(
        !refs
            .iter()
            .any(|(module, _)| *module == "external.ignore.Value")
    );
    assert!(hints.warnings.is_empty(), "{:?}", hints.warnings);
}

#[test]
fn django_custom_settings_only_reference_existing_first_party_values() {
    let hints = django_project(&[
        ("mysite/__init__.py", ""),
        ("mysite/plugins.py", ""),
        ("mysite/backend.py", ""),
        ("mysite/key.py", ""),
        ("mysite/function.py", ""),
        (
            "mysite/settings.py",
            concat!(
                "INSTALLED_APPS = []\n",
                "BUILTIN_PLUGINS = ['mysite.plugins.Plugin', 'mysite.plugins']\n",
                "if True:\n    PRIVATE_STORAGE = {'backend': 'mysite.backend.Storage'}\n",
                "OTHER = {'mysite.key': 'unknown.backend.Class', 'missing': 'mysite.missing.AppConfig', 'url': 'https://mysite.key', 'name': 'mysite.missing.Unknown', 'package': 'mysite', 'invalid': 'mysite.backend.bad-name'}\n",
                "def helper():\n    BACKEND = 'mysite.function.Factory'\n",
            ),
        ),
    ]);
    let refs: Vec<_> = django_contrib(&hints)
        .module_refs
        .iter()
        .map(|reference| {
            (
                reference.module.as_str(),
                reference.origin.label.as_str(),
                reference.origin.line,
            )
        })
        .collect();
    assert_eq!(
        refs,
        [
            ("mysite.plugins.Plugin", "BUILTIN_PLUGINS", Some(2)),
            ("mysite.plugins", "BUILTIN_PLUGINS", Some(2)),
            ("mysite.backend.Storage", "PRIVATE_STORAGE", Some(4)),
        ]
    );
}

#[test]
fn django_import_settings_keep_literals_and_report_unread_values() {
    for field in [
        "AUTHENTICATION_BACKENDS = ['mysite.backend.Backend', computed()]",
        "PASSWORD_HASHERS = ('mysite.backend.Backend', computed())",
        "STORAGES = {'default': {'BACKEND': backend_name}}",
        "TEMPLATES = [{'OPTIONS': {'context_processors': processors}}]",
        "LOGGING = {'handlers': handlers}",
        "STORAGES = {'default': {'BACKEND': 'mysite.backend.Backend'}, **other}",
        "STORAGES[key] = {'BACKEND': backend_name}",
        "other, AUTHENTICATION_BACKENDS = computed()",
    ] {
        let settings = format!("INSTALLED_APPS = []\n{field}\n");
        let hints = django_project(&[("mysite/settings.py", &settings)]);
        assert!(
            !django_contrib(&hints).unread_settings.is_empty(),
            "{field}"
        );
        assert!(
            hints.warnings.iter().any(|warning| matches!(warning,
                PluginsWarning::DjangoSettingsUnread { path, line: Some(2), .. }
                    if path == "mysite/settings.py"
            )),
            "{field}: {:?}",
            hints.warnings
        );
        if field.starts_with("AUTHENTICATION_BACKENDS") || field.starts_with("PASSWORD_HASHERS") {
            assert!(
                django_contrib(&hints)
                    .module_refs
                    .iter()
                    .any(|reference| reference.module == "mysite.backend.Backend")
            );
        }
    }
}

#[test]
fn django_unread_backend_settings_name_their_setting() {
    for (field, backend) in [
        (
            "DATABASES = {'default': {'ENGINE': engine}}",
            Some("DATABASES"),
        ),
        (
            "DATABASES = {'default': dj_database_url.config()}",
            Some("DATABASES"),
        ),
        ("DATABASES['default'] = config", Some("DATABASES")),
        ("CACHES = {'default': {'BACKEND': backend}}", Some("CACHES")),
        ("STORAGES = {'default': {'BACKEND': backend}}", None),
    ] {
        let settings = format!("INSTALLED_APPS = []\n{field}\n");
        let hints = django_project(&[("mysite/settings.py", &settings)]);
        let backends: Vec<_> = django_contrib(&hints)
            .unread_settings
            .iter()
            .map(|unread| unread.backend.as_deref())
            .collect();
        assert_eq!(backends, [backend], "{field}");
    }
    // An omitted ENGINE is Django's default, not an unread value.
    let hints = django_project(&[(
        "mysite/settings.py",
        "INSTALLED_APPS = []\nDATABASES = {'default': {'NAME': name}}\n",
    )]);
    assert!(django_contrib(&hints).unread_settings.is_empty());
}

#[test]
fn django_import_settings_require_named_keys_in_nested_containers() {
    for storage in ["['external.not_a_backend']", "('external.not_a_backend',)"] {
        let settings = format!("INSTALLED_APPS = []\nSTORAGES = {{'default': {storage}}}\n");
        let hints = django_project(&[("mysite/settings.py", &settings)]);
        let contrib = django_contrib(&hints);
        assert!(
            contrib.module_refs.is_empty(),
            "{storage}: {:?}",
            contrib.module_refs
        );
        assert!(!contrib.unread_settings.is_empty(), "{storage}");
    }
}

#[test]
fn django_unpacked_settings_do_not_make_other_values_import_evidence() {
    for target in [
        "other, AUTHENTICATION_BACKENDS",
        "[other, AUTHENTICATION_BACKENDS]",
    ] {
        let settings = format!(
            "INSTALLED_APPS = []\n{target} = ('external.other.Value', ['external.auth.Backend'])\n"
        );
        let hints = django_project(&[("mysite/settings.py", &settings)]);
        let contrib = django_contrib(&hints);
        assert!(contrib.module_refs.is_empty(), "{:?}", contrib.module_refs);
        assert!(!contrib.unread_settings.is_empty(), "{target}");
    }
}

#[test]
fn django_import_settings_ignore_updates_outside_import_slots() {
    let hints = django_project(&[(
        "mysite/settings.py",
        "INSTALLED_APPS = []\nSTORAGES = {'default': {'BACKEND': 'external.storage.Backend'}}\nSTORAGES['default']['OPTIONS'] = {'bucket': 'example.bucket'}\n",
    )]);
    let refs: Vec<_> = django_contrib(&hints)
        .module_refs
        .iter()
        .map(|reference| reference.module.as_str())
        .collect();
    assert_eq!(refs, ["external.storage.Backend"]);
    assert!(hints.warnings.is_empty(), "{:?}", hints.warnings);
}

#[test]
fn django_import_settings_preserve_existing_reference_kinds() {
    let hints = django_project(&[
        ("mysite/__init__.py", ""),
        ("mysite/backend.py", ""),
        (
            "mysite/settings.py",
            concat!(
                "INSTALLED_APPS = ['mysite.backend']\n",
                "MIDDLEWARE = ['mysite.backend.Backend']\n",
                "ROOT_URLCONF = 'mysite.backend'\n",
                "WSGI_APPLICATION = 'mysite.backend:application'\n",
                "ASGI_APPLICATION = 'mysite.backend:application'\n",
            ),
        ),
    ]);
    let contrib = django_contrib(&hints);
    let refs: Vec<_> = contrib
        .module_refs
        .iter()
        .map(|reference| (reference.module.as_str(), reference.origin.label.as_str()))
        .collect();
    assert_eq!(
        refs,
        [
            ("mysite.backend", "INSTALLED_APPS"),
            ("mysite.backend.Backend", "MIDDLEWARE"),
            ("mysite.backend", "ROOT_URLCONF"),
        ]
    );
    assert_eq!(contrib.symbol_refs.len(), 2);
    assert!(hints.warnings.is_empty(), "{:?}", hints.warnings);
}

#[test]
fn django_server_applications_read_dotted_targets_or_report_unknown_values() {
    for field in ["WSGI_APPLICATION", "ASGI_APPLICATION"] {
        for (value, referenced, unread) in [
            ("'mysite.server.application'", true, false),
            ("'mysite.server:application'", true, false),
            ("None", false, false),
            ("''", false, false),
            ("build_application()", false, true),
            ("APPLICATION", false, true),
            ("'mysite.server.bad-name'", false, true),
            ("'not_a_target'", false, true),
        ] {
            let settings = format!("INSTALLED_APPS = []\n{field} = {value}\n");
            let hints = django_project(&[
                ("mysite/__init__.py", ""),
                ("mysite/server.py", "application = None\n"),
                ("mysite/settings.py", &settings),
            ]);
            let contrib = django_contrib(&hints);
            assert_eq!(
                contrib.symbol_refs.len(),
                usize::from(referenced),
                "{field} = {value}"
            );
            assert_eq!(
                !contrib.unread_settings.is_empty(),
                unread,
                "{field} = {value}"
            );
            if let Some(reference) = contrib.symbol_refs.first() {
                assert_eq!(reference.module, "mysite.server");
                assert_eq!(reference.symbol, "application");
                assert_eq!(reference.origin.line, Some(2));
                assert_eq!(reference.origin.label, field);
            }
        }
    }
}

#[test]
fn django_import_settings_follow_star_imports_and_module_assignments() {
    let hints = django_project(&[
        ("mysite/__init__.py", ""),
        ("mysite/backend.py", ""),
        ("mysite/settings.py", "from .base import *\n"),
        (
            "mysite/base.py",
            concat!(
                "INSTALLED_APPS = []\n",
                "AUTHENTICATION_BACKENDS: list[str] = ['mysite.backend.Backend']\n",
                "BUILTIN_PLUGINS = []\n",
                "BUILTIN_PLUGINS += ('mysite.backend.Plugin',)\n",
                "EMPTY: str\n",
                "IGNORED = lambda: 'mysite.backend.Lambda'\n",
                "class Config:\n    EMAIL_BACKEND = 'mysite.backend.Class'\n",
                "TEMPLATES = [{'OPTIONS': {}}]\n",
                "STORAGES = {}\n",
                "STORAGES['default'] = {'BACKEND': 'external.storage.Backend'}\n",
                "STORAGES['staticfiles'] = {}\n",
                "STORAGES['staticfiles']['BACKEND'] = 'external.static.Backend'\n",
                "TEMPLATES[0]['OPTIONS']['context_processors'] = ['external.context.user']\n",
            ),
        ),
    ]);
    let refs: Vec<_> = django_contrib(&hints)
        .module_refs
        .iter()
        .map(|reference| {
            (
                reference.module.as_str(),
                reference.origin.file.as_str(),
                reference.origin.line,
            )
        })
        .collect();
    assert_eq!(
        refs,
        [
            ("mysite.backend.Backend", "mysite/base.py", Some(2)),
            ("mysite.backend.Plugin", "mysite/base.py", Some(4)),
            ("external.storage.Backend", "mysite/base.py", Some(11)),
            ("external.static.Backend", "mysite/base.py", Some(13)),
            ("external.context.user", "mysite/base.py", Some(14)),
        ]
    );
    assert!(hints.warnings.is_empty(), "{:?}", hints.warnings);
}

/// A project depending on Django has its settings read without `manage.py`.
#[test]
fn django_reads_settings_found_without_a_setter() {
    let hints = django_project(&[("mysite/settings.py", "INSTALLED_APPS = [\"myapp\"]\n")]);
    assert_eq!(entry_paths(django_contrib(&hints)), ["mysite/settings.py"]);
}

#[test]
fn django_ambiguous_settings_names_the_other_candidates() {
    let settings = "INSTALLED_APPS = []\n";
    let hints = django_project(&[
        ("alpha/settings.py", settings),
        ("beta/settings.py", settings),
    ]);
    assert_eq!(
        hints.warnings,
        [PluginsWarning::AmbiguousSettings {
            chosen: "alpha/settings.py".to_owned(),
            candidates: vec!["beta/settings.py".to_owned()],
        }]
    );
}

/// A nested `wsgi.py` names the settings module when no root `manage.py` does.
#[test]
fn django_settings_module_from_a_nested_wsgi() {
    let hints = django_project(&[
        ("mysite/__init__.py", ""),
        (
            "mysite/wsgi.py",
            "import os\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"mysite.conf\")\n",
        ),
        ("mysite/conf.py", "INSTALLED_APPS = [\"myapp\"]\n"),
    ]);
    assert_eq!(entry_paths(django_contrib(&hints)), ["mysite/conf.py"]);
}

/// A runtime `wsgi.py` outranks a test one.
#[test]
fn django_settings_module_prefers_a_runtime_setter() {
    let hints = django_project(&[
        (
            "tests/wsgi.py",
            "import os\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"tests.settings\")\n",
        ),
        ("tests/settings.py", "INSTALLED_APPS = []\n"),
        ("mysite/__init__.py", ""),
        (
            "mysite/wsgi.py",
            "import os\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"mysite.conf\")\n",
        ),
        ("mysite/conf.py", "INSTALLED_APPS = [\"myapp\"]\n"),
    ]);
    assert_eq!(entry_paths(django_contrib(&hints)), ["mysite/conf.py"]);
}

/// A template's placeholder and a view module named `settings.py` do not
/// hide the settings a root `runtests.py` names (wagtail, #979).
#[test]
fn django_settings_module_from_runtests_past_a_template_placeholder() {
    let hints = django_project(&[
        ("wagtail/__init__.py", ""),
        (
            "wagtail/project_template/project_name/wsgi.py",
            "import os\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"{{ project_name }}.settings.dev\")\n",
        ),
        (
            "wagtail/contrib/views/settings.py",
            "from django.shortcuts import redirect\n\n\ndef index(request):\n    return redirect(\"/\")\n",
        ),
        ("wagtail/test/__init__.py", ""),
        (
            "wagtail/test/settings.py",
            "ROOT_URLCONF = \"wagtail.test.urls\"\nINSTALLED_APPS = []\n",
        ),
        (
            "runtests.py",
            "import os\nos.environ[\"DJANGO_SETTINGS_MODULE\"] = \"wagtail.test.settings\"\n",
        ),
    ]);
    assert_eq!(
        entry_paths(django_contrib(&hints)),
        ["wagtail/test/settings.py"]
    );
    assert_eq!(hints.warnings, []);
}

/// Each setter is read once: the root `manage.py`, then the root setters the
/// walk did not take (#979).
#[test]
fn django_settings_module_reads_each_root_setter_once() {
    let setter = |module: &str| {
        format!("import os\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"{module}\")\n")
    };
    let hints = django_project(&[
        ("manage.py", &setter("mysite.settings")),
        ("runtests.py", &setter("mysite.test_settings")),
        ("mysite/__init__.py", ""),
        ("mysite/settings.py", "INSTALLED_APPS = []\n"),
        ("mysite/test_settings.py", "INSTALLED_APPS = []\n"),
    ]);
    let refs: Vec<(&str, &str)> = django_contrib(&hints)
        .module_refs
        .iter()
        .filter(|reference| reference.origin.label == "DJANGO_SETTINGS_MODULE")
        .map(|reference| (reference.module.as_str(), reference.origin.file.as_str()))
        .collect();
    assert_eq!(
        refs,
        [
            ("mysite.settings", "manage.py"),
            ("mysite.test_settings", "runtests.py"),
        ]
    );
}

/// pytest-django's ini option names the settings module (#979).
#[test]
fn django_settings_module_from_the_pytest_config() {
    let hints = django_project(&[
        (
            "pytest.ini",
            "[pytest]\nDJANGO_SETTINGS_MODULE = acme.conf\n",
        ),
        ("acme/__init__.py", ""),
        ("acme/conf.py", "INSTALLED_APPS = [\"acme\"]\n"),
    ]);
    assert_eq!(entry_paths(django_contrib(&hints)), ["acme/conf.py"]);
}

/// A sample project under `examples/` and a `settings.py` that sets no
/// Django field are not this project's settings (celery, #979).
#[test]
fn django_skips_example_and_non_settings_candidates() {
    let hints = django_project(&[
        (
            "examples/django/manage.py",
            "import os\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"proj.settings\")\n",
        ),
        ("examples/django/proj/settings.py", "INSTALLED_APPS = []\n"),
        ("examples/gateway/settings.py", "ROOT_URLCONF = \"urls\"\n"),
        ("acme/__init__.py", ""),
        (
            "acme/views/settings.py",
            "def index(request):\n    return None\n",
        ),
        // In the scanned package, an example's setter is skipped all the same.
        (
            "acme/example/wsgi.py",
            "import os\nos.environ.setdefault(\"DJANGO_SETTINGS_MODULE\", \"acme.example.settings\")\n",
        ),
        ("acme/example/settings.py", "INSTALLED_APPS = []\n"),
    ]);
    assert_eq!(entry_paths(django_contrib(&hints)), Vec::<String>::new());
    assert_eq!(hints.warnings, []);
}

/// django-configurations sets `INSTALLED_APPS` in a class body and
/// django-split-settings `include`s it: both files are settings candidates.
#[test]
fn django_finds_class_and_split_settings_candidates() {
    for settings in [
        "from configurations import Configuration\n\nclass Dev(Configuration):\n    INSTALLED_APPS = []\n",
        "from split_settings.tools import include\n\ninclude(\"components/base.py\")\n",
    ] {
        let hints = django_project(&[("mysite/__init__.py", ""), ("mysite/settings.py", settings)]);
        assert!(
            entry_paths(django_contrib(&hints)).contains(&"mysite/settings.py"),
            "{settings}"
        );
    }
}

/// A settings module star-imported twice is read once.
#[test]
fn django_reads_a_twice_star_imported_settings_module_once() {
    let hints = django_project(&[
        ("mysite/__init__.py", ""),
        (
            "mysite/settings.py",
            "from mysite.base import *\nfrom mysite.local import *\n",
        ),
        ("mysite/local.py", "from mysite.base import *\n"),
        ("mysite/base.py", "INSTALLED_APPS = [\"myapp\"]\n"),
    ]);
    let apps = django_contrib(&hints)
        .module_refs
        .iter()
        .filter(|reference| reference.module == "myapp")
        .count();
    assert_eq!(apps, 1);
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

/// A reusable app has no settings of its own, yet every project that installs
/// it runs its migrations (DRF's `authtoken/migrations/`, #979).
#[test]
fn django_reusable_app_migrations_framework_used() {
    let hints = django_project(&[
        ("app/__init__.py", ""),
        ("app/settings.py", "API_DEFAULTS = {}\n"),
        ("app/migrations/__init__.py", ""),
        ("app/migrations/0001_initial.py", ""),
    ]);
    let contrib = django_contrib(&hints);
    assert_eq!(entry_paths(contrib), Vec::<&str>::new());
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
    assert_eq!(hints.contributions, []);
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
            .any(|usage| usage.binary == "pytest"
                && usage.origin.file == ".github/workflows/ci.yml"
                && usage.origin.line == Some(15))
    );
    // `uv run ruff` runs ruff; uv itself is an environment manager, not a dependency.
    assert!(
        contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "ruff" && usage.origin.line == Some(15))
    );
    assert!(
        !contrib
            .binary_usages
            .iter()
            .any(|usage| usage.binary == "uv")
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

fn symbol_ref_keys(
    contrib: &chokkin::internals::PluginContribution,
) -> Vec<(&str, &str, &str, Option<u32>, &str)> {
    let mut keys: Vec<_> = contrib
        .symbol_refs
        .iter()
        .map(|reference| {
            (
                reference.module.as_str(),
                reference.symbol.as_str(),
                reference.origin.file.as_str(),
                reference.origin.line,
                reference.origin.label.as_str(),
            )
        })
        .collect();
    keys.sort_unstable();
    keys
}

fn binary_usage_keys(
    contrib: &chokkin::internals::PluginContribution,
) -> Vec<(&str, &str, Option<u32>)> {
    let mut keys: Vec<_> = contrib
        .binary_usages
        .iter()
        .map(|usage| {
            (
                usage.binary.as_str(),
                usage.origin.file.as_str(),
                usage.origin.line,
            )
        })
        .collect();
    keys.sort_unstable();
    keys
}

fn module_ref_keys(
    contrib: &chokkin::internals::PluginContribution,
) -> Vec<(&str, &str, Option<u32>, &str)> {
    let mut keys: Vec<_> = contrib
        .module_refs
        .iter()
        .map(|reference| {
            (
                reference.module.as_str(),
                reference.origin.file.as_str(),
                reference.origin.line,
                reference.origin.label.as_str(),
            )
        })
        .collect();
    keys.sort_unstable();
    keys
}

fn used_symbol_keys(contrib: &chokkin::internals::PluginContribution) -> Vec<(&str, &str)> {
    let mut keys: Vec<_> = contrib
        .used_symbols
        .iter()
        .map(|reference| (reference.module.as_str(), reference.symbol.as_str()))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// A bare module after `FLASK_APP=` / `--app` is the module Flask searches for
/// its app or factory, so it is a module reference and those names are used;
/// the quoted `module:factory()` form names the factory (#851).
#[test]
fn flask_plugin_reads_bare_app_modules() {
    let hints = extract_fixture("flask_bare_module");
    let contrib = plugin_contrib(&hints, PluginId::Flask);
    assert_eq!(
        module_ref_keys(contrib),
        [
            ("web.app", ".flaskenv", Some(1), "FLASK_APP"),
            ("web.dev", "bin/dev.sh", Some(2), "flask --app"),
            ("web.wsgi", "scripts/run.sh", Some(2), "flask --app"),
        ]
    );
    let used = used_symbol_keys(contrib);
    for module in ["web.app", "web.dev", "web.wsgi"] {
        for name in ["app", "application", "create_app", "make_app"] {
            assert!(used.contains(&(module, name)), "{module}:{name} {used:?}");
        }
    }
    assert_eq!(used.len(), 12, "{used:?}");
    assert_eq!(
        symbol_ref_keys(contrib),
        [(
            "web.factory",
            "create_app",
            "bin/serve.sh",
            Some(2),
            "flask --app"
        )]
    );
}

/// A file path after `FLASK_APP=` / `--app` names the module Flask's
/// `prepare_import` imports: a `.py` file or a package directory, with or
/// without `:attr` (#866). A path naming no discovered file, leaving the
/// root, or built from a variable is not read.
#[test]
fn flask_plugin_reads_app_file_paths() {
    let hints = extract_fixture("flask_app_paths");
    let contrib = plugin_contrib(&hints, PluginId::Flask);
    assert_eq!(
        module_ref_keys(contrib),
        [
            ("web.app", ".flaskenv", Some(1), "FLASK_APP"),
            ("web.pkg", "bin/pkg.sh", Some(2), "flask --app"),
            ("web.wsgi", "scripts/run.sh", Some(2), "flask --app"),
        ]
    );
    assert_eq!(used_symbol_keys(contrib).len(), 12);
    assert_eq!(
        symbol_ref_keys(contrib),
        [(
            "web.factory",
            "make_app",
            "bin/serve.sh",
            Some(2),
            "flask --app"
        )]
    );
    assert_eq!(entry_paths(contrib), Vec::<&str>::new());
}

/// A path to a file the layout names no module for, such as a script at the
/// root of a project without a package, is an entry root: Flask imports it
/// from its own directory (#866). So is such a file named as a module, since
/// Flask puts the working directory on `sys.path` for it too.
#[test]
fn flask_plugin_roots_app_files_without_module_names() {
    let hints = extract_fixture("flask_root_app");
    let contrib = plugin_contrib(&hints, PluginId::Flask);
    let mut entries: Vec<_> = contrib
        .entries
        .iter()
        .map(|entry| {
            (
                entry.spec.path.as_str(),
                entry.spec.symbol.as_deref(),
                entry.origin.file.as_str(),
                entry.origin.label.as_str(),
            )
        })
        .collect();
    entries.sort_unstable();
    assert_eq!(
        entries,
        [
            ("admin.py", Some("app"), ".flaskenv", "FLASK_APP"),
            ("hello.py", None, "scripts/run.sh", "flask --app"),
        ]
    );
    assert_eq!(module_ref_keys(contrib), []);
    assert_eq!(symbol_ref_keys(contrib), []);
}

/// An app file keeps the context of the directory it is in (#866).
#[test]
fn flask_plugin_app_file_entries_keep_their_context() {
    let hints = extract_fixture("flask_dev_app");
    let contrib = plugin_contrib(&hints, PluginId::Flask);
    let entries: Vec<_> = contrib
        .entries
        .iter()
        .map(|entry| (entry.spec.path.as_str(), entry.context))
        .collect();
    assert_eq!(entries, [("scripts/app.py", FileContext::Dev)]);
}

/// `celery -A pkg.mod` names the module Celery searches for `app` / `celery`,
/// or the attribute `mod` of `pkg` (#851).
/// A package without either has Celery import its `celery` submodule (#865).
#[test]
fn celery_plugin_reads_bare_app_modules() {
    let hints = extract_fixture("celery_bare_module");
    let contrib = plugin_contrib(&hints, PluginId::Celery);
    assert_eq!(
        module_ref_keys(contrib),
        [
            ("worker.celery", "bin/worker.sh", Some(2), "celery app"),
            (
                "worker.celery.celery",
                "bin/worker.sh",
                Some(2),
                "celery app"
            ),
            (
                "worker.instance",
                "pyproject.toml",
                None,
                "project.scripts.worker"
            ),
            (
                "worker.instance.celery",
                "pyproject.toml",
                None,
                "project.scripts.worker"
            ),
            ("worker.monitor", "scripts/flower.sh", Some(2), "celery app"),
            (
                "worker.monitor.celery",
                "scripts/flower.sh",
                Some(2),
                "celery app"
            ),
        ]
    );
    assert_eq!(
        used_symbol_keys(contrib),
        [
            ("worker", "celery"),
            ("worker", "instance"),
            ("worker", "monitor"),
            ("worker.celery", "app"),
            ("worker.celery", "celery"),
            ("worker.celery.celery", "app"),
            ("worker.celery.celery", "celery"),
            ("worker.instance", "app"),
            ("worker.instance", "celery"),
            ("worker.instance.celery", "app"),
            ("worker.instance.celery", "celery"),
            ("worker.monitor", "app"),
            ("worker.monitor", "celery"),
            ("worker.monitor.celery", "app"),
            ("worker.monitor.celery", "celery"),
        ]
    );
    assert!(contrib.symbol_refs.is_empty(), "{:?}", contrib.symbol_refs);
    assert_eq!(binary_usage_keys(contrib).len(), 3);
}

#[test]
fn flask_plugin_reads_app_from_shell_scripts() {
    // `scripts/run.sh` spells `--app X`, `bin/serve.sh` spells `--app=X`; the
    // shebang and `set -e` lines name no app.
    let hints = extract_fixture("flask_shell_scripts");
    let contrib = plugin_contrib(&hints, PluginId::Flask);
    assert_eq!(
        symbol_ref_keys(contrib),
        [
            (
                "myapp.factory",
                "create_app",
                "scripts/run.sh",
                Some(3),
                "flask --app"
            ),
            ("myapp.wsgi", "app", "bin/serve.sh", Some(2), "flask --app"),
        ]
    );
    assert_eq!(
        binary_usage_keys(contrib),
        [
            ("flask", "bin/serve.sh", Some(2)),
            ("flask", "scripts/run.sh", Some(3)),
        ]
    );
}

#[test]
fn celery_plugin_reads_app_from_shell_scripts() {
    // `bin/worker.sh` spells `-A X`, `scripts/beat.sh` the attached `-AX`; the
    // shebang and `echo` lines name no app.
    let hints = extract_fixture("celery_shell_scripts");
    let contrib = plugin_contrib(&hints, PluginId::Celery);
    assert_eq!(
        symbol_ref_keys(contrib),
        [
            (
                "worker.beat",
                "app",
                "scripts/beat.sh",
                Some(2),
                "celery app"
            ),
            (
                "worker.celery",
                "app",
                "bin/worker.sh",
                Some(3),
                "celery app"
            ),
        ]
    );
    assert_eq!(
        binary_usage_keys(contrib),
        [
            ("celery", "bin/worker.sh", Some(3)),
            ("celery", "scripts/beat.sh", Some(2)),
        ]
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
    // Extensions added after the assignment (#719).
    for module in ["sphinx_copybutton", "sphinx.ext.napoleon", "sphinx_design"] {
        assert!(
            contrib
                .module_refs
                .iter()
                .any(|reference| reference.module == module),
            "{module}"
        );
    }
    // Sphinx ships `alabaster`; only a declared theme is a module (#809).
    assert!(
        !contrib
            .module_refs
            .iter()
            .any(|reference| ["_build", "alabaster"].contains(&reference.module.as_str()))
    );
}

/// Each extension carries the line of the string that names it (#830).
#[test]
fn sphinx_extensions_carry_their_string_line() {
    let hints = extract_fixture("sphinx_docs");
    let contrib = plugin_contrib(&hints, PluginId::Sphinx);
    let lines: Vec<(&str, Option<u32>)> = contrib
        .module_refs
        .iter()
        .map(|reference| (reference.module.as_str(), reference.origin.line))
        .collect();
    assert_eq!(
        lines,
        [
            ("sphinx.ext.autodoc", Some(3)),
            ("myst_parser", Some(4)),
            ("sphinx_copybutton", Some(6)),
            ("sphinx.ext.napoleon", Some(7)),
            ("sphinx_design", Some(8)),
        ]
    );
}

/// `docs/conf.py` that star-imports a first-party module reads that
/// module's `extensions`, resolved through its own imports, at the star
/// import's line in `conf.py`, and merges them with its own (#1062).
#[test]
fn sphinx_conf_reads_the_extensions_of_a_star_imported_module() {
    let hints = extract_fixture("sphinx_star_conf");
    let contrib = plugin_contrib(&hints, PluginId::Sphinx);
    let refs: Vec<(&str, &str, Option<u32>)> = contrib
        .module_refs
        .iter()
        .map(|reference| {
            (
                reference.module.as_str(),
                reference.origin.file.as_str(),
                reference.origin.line,
            )
        })
        .collect();
    assert_eq!(
        refs,
        [
            ("sphinx_design", "docs/conf.py", Some(3)),
            ("sphinx.ext.autodoc", "docs/conf.py", Some(1)),
            ("sphinx_copybutton", "docs/conf.py", Some(1)),
        ]
    );
    assert_eq!(contrib.unread_settings, []);
    assert_eq!(hints.warnings, []);
}

/// A discovered `conf.py` beside a root document is a docs entry wherever it
/// is; one with no root document beside it is no Sphinx conf (#814).
#[test]
fn sphinx_conf_beside_a_root_document_is_a_docs_entry() {
    let hints = extract_fixture("sphinx_root_document");
    let contrib = plugin_contrib(&hints, PluginId::Sphinx);
    let entries: Vec<(&str, FileContext, &str)> = contrib
        .entries
        .iter()
        .map(|entry| {
            (
                entry.spec.path.as_str(),
                entry.context,
                entry.origin.label.as_str(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [(
            "docs/api/source/conf.py",
            FileContext::Docs,
            "conf.py beside index.rst"
        )]
    );
    let modules: Vec<&str> = contrib
        .module_refs
        .iter()
        .map(|reference| reference.module.as_str())
        .collect();
    assert_eq!(modules, ["sphinx_click"]);
    assert_eq!(contrib.scanned_files, ["docs/api/source/conf.py"]);
}

/// Plugin hints for a project with `files` written.
fn project_hints(files: &[(&str, &str)]) -> chokkin::internals::PluginHints {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path();
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        std::fs::write(path, text).expect("write file");
    }
    extract_at(root)
}

/// `docs/conf.py` beside a root document is one entry, and another module
/// beside it is none (#814).
#[test]
fn sphinx_docs_conf_beside_a_root_document_is_one_entry() {
    let hints = project_hints(&[
        (
            "pyproject.toml",
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\ndependencies = [\"sphinx\"]\n\n[tool.chokkin.plugins]\nsphinx = true\n",
        ),
        ("docs/conf.py", ""),
        ("docs/index.rst", "Docs\n====\n"),
        ("docs/helpers.py", ""),
    ]);
    let contrib = plugin_contrib(&hints, PluginId::Sphinx);
    assert_eq!(entry_paths(contrib), ["docs/conf.py"]);
}

/// A star import not followed is no gap when a star-imported module that is
/// followed assigns `extensions` (#1062).
#[test]
fn sphinx_star_import_not_followed_is_no_gap_where_a_followed_one_assigns() {
    let hints = project_hints(&[
        (
            "pyproject.toml",
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\ndependencies = [\"sphinx\"]\n\n[tool.chokkin.plugins]\nsphinx = true\n",
        ),
        (
            "docs/conf.py",
            "from common.provider_conf import *\nfrom sphinx_base_conf import *\n",
        ),
        ("src/common/__init__.py", ""),
        ("src/common/provider_conf.py", "extensions = [\"a\"]\n"),
    ]);
    let contrib = plugin_contrib(&hints, PluginId::Sphinx);
    assert_eq!(contrib.unread_settings, []);
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

fn alembic_entries(files: &[(&str, &str)]) -> Vec<String> {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"alembic\"]\n\n[tool.chokkin.plugins]\nalembic = true\n",
    )
    .expect("write pyproject");
    for (file, text) in files {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, text).expect("write fixture");
    }
    let hints = extract_at(root);
    let mut paths: Vec<String> = entry_paths(plugin_contrib(&hints, PluginId::Alembic))
        .into_iter()
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths
}

/// `script_location` names the migration environment alembic loads by path,
/// from an `alembic.ini` inside the package (#667); revisions in
/// subdirectories load only with `recursive_version_locations`.
#[test]
fn alembic_ini_script_location_roots_env_and_revisions() {
    let project = |ini: &str| {
        alembic_entries(&[
            ("src/acme/__init__.py", ""),
            ("src/acme/alembic.ini", ini),
            ("src/acme/migrations/env.py", ""),
            ("src/acme/migrations/utils.py", ""),
            ("src/acme/migrations/versions/a1_init.py", ""),
            ("src/acme/migrations/versions/2024/b2_next.py", ""),
        ])
    };
    assert_eq!(
        project("[alembic]\nscript_location = %(here)s/migrations\n"),
        [
            "src/acme/migrations/env.py",
            "src/acme/migrations/versions/a1_init.py",
        ]
    );
    assert_eq!(
        project(
            "[alembic]\nscript_location = %(here)s/migrations\nrecursive_version_locations = true\n"
        ),
        [
            "src/acme/migrations/env.py",
            "src/acme/migrations/versions/2024/b2_next.py",
            "src/acme/migrations/versions/a1_init.py",
        ]
    );
}

/// `version_path_separator = os` splits `version_locations` on `:`.
#[test]
fn alembic_version_locations_follow_path_separator() {
    let paths = alembic_entries(&[
        (
            "alembic.ini",
            "[alembic]\nscript_location = db\nversion_path_separator = os  # Use os.pathsep.\nversion_locations = db/a:db/b\n",
        ),
        ("db/env.py", ""),
        ("db/a/a1_init.py", ""),
        ("db/b/b2_next.py", ""),
    ]);
    assert_eq!(paths, ["db/a/a1_init.py", "db/b/b2_next.py", "db/env.py"]);
}

/// A config whose `script_location` resolves nowhere keeps the fixed
/// `alembic/env.py`; `%(here)s` never falls back to the root.
#[test]
fn alembic_unresolved_location_keeps_default_env() {
    let paths = alembic_entries(&[
        ("src/acme/__init__.py", ""),
        (
            "src/acme/alembic.ini",
            "[alembic]\nscript_location = %(here)s/db\n",
        ),
        ("db/env.py", ""),
        ("alembic/env.py", ""),
    ]);
    assert_eq!(paths, ["alembic/env.py"]);
}

/// `pkg:dir` resolves under the package; `version_locations` replaces
/// `<script_location>/versions`.
#[test]
fn alembic_package_location_and_version_locations() {
    let paths = alembic_entries(&[
        ("src/acme/__init__.py", ""),
        ("src/acme/db/__init__.py", ""),
        (
            "alembic.ini",
            "[alembic]\nscript_location = acme.db:migrations\nversion_locations = %(here)s/src/acme/revs  # note\n",
        ),
        ("src/acme/db/migrations/env.py", ""),
        ("src/acme/db/migrations/versions/a1_init.py", ""),
        ("src/acme/revs/b2_next.py", ""),
    ]);
    assert_eq!(
        paths,
        ["src/acme/db/migrations/env.py", "src/acme/revs/b2_next.py"]
    );
}

/// The origin points at the `script_location` line; `pkg:/dir` is an absolute
/// path, not a package resource, so it resolves nowhere.
#[test]
fn alembic_ini_origin_line_and_absolute_package_dir() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path();
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"alembic\"]\n\n[tool.chokkin.plugins]\nalembic = true\n",
        ),
        ("src/acme/__init__.py", ""),
        (
            "src/acme/alembic.ini",
            "[alembic]\n# comment\nscript_location = %(here)s/migrations\n",
        ),
        ("src/acme/migrations/env.py", ""),
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, text).expect("write fixture");
    }
    let hints = extract_at(root);
    let contrib = plugin_contrib(&hints, PluginId::Alembic);
    assert_eq!(entry_paths(contrib), ["src/acme/migrations/env.py"]);
    assert_eq!(contrib.entries[0].origin.line, Some(3));

    let paths = alembic_entries(&[
        ("src/acme/__init__.py", ""),
        (
            "alembic.ini",
            "[alembic]\nscript_location = acme:/migrations\n",
        ),
        ("src/acme/migrations/env.py", ""),
    ]);
    assert!(paths.is_empty(), "{paths:?}");
}

/// alembic 1.16 reads `[tool.alembic]` from `pyproject.toml`, relative paths
/// fall back to the root, and with no config the fixed `alembic/env.py` stays.
#[test]
fn alembic_pyproject_location_and_default_env() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"alembic\"]\n\n[tool.chokkin.plugins]\nalembic = true\n\n[tool.alembic]\nscript_location = \"db\"\n",
    )
    .expect("write pyproject");
    for file in ["db/env.py", "db/versions/a1_init.py", "alembic/env.py"] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, "").expect("write fixture");
    }
    let hints = extract_at(root);
    let contrib = plugin_contrib(&hints, PluginId::Alembic);
    assert_eq!(
        entry_paths(contrib),
        ["db/env.py", "db/versions/a1_init.py"]
    );
    assert_eq!(contrib.entries[0].origin.line, Some(10));

    let paths = alembic_entries(&[("alembic/env.py", "")]);
    assert_eq!(paths, ["alembic/env.py"]);
}

#[test]
fn full_pipeline_step5() {
    let hints = extract_fixture("django_manage");
    let plugins: Vec<_> = hints
        .contributions
        .iter()
        .map(|contrib| contrib.plugin)
        .collect();
    assert_eq!(
        plugins,
        [PluginId::Pytest, PluginId::Django, PluginId::Fastapi]
    );

    // The fixture has no tests and no FastAPI app, so only Django contributes.
    assert_eq!(entry_paths(pytest_contrib(&hints)), Vec::<&str>::new());
    assert_eq!(entry_paths(fastapi_contrib(&hints)), Vec::<&str>::new());
    let mut django_entries = entry_paths(django_contrib(&hints));
    django_entries.sort_unstable();
    assert_eq!(
        django_entries,
        ["manage.py", "mysite/settings.py", "mysite/urls.py"]
    );
    assert_eq!(hints.warnings, []);
}

#[test]
fn partial_settings_warns() {
    // An `INSTALLED_APPS` not read may install any app: a gap at its line.
    let hints = extract_fixture("django_partial_settings");
    assert!(hints.warnings.iter().any(|warning| matches!(warning,
        PluginsWarning::DjangoSettingsUnread { path, line: Some(1), .. }
            if path == "mysite/settings.py"
    )));
    assert!(!django_contrib(&hints).unread_settings.is_empty());
    let hints = django_project(&[(
        "mysite/settings.py",
        "INSTALLED_APPS = []\nMIDDLEWARE = build()\n",
    )]);
    assert!(
        hints
            .warnings
            .iter()
            .any(|warning| { matches!(warning, PluginsWarning::PartialSettingsParse { .. }) })
    );
}

fn extract_fixture_from_deps(name: &str) -> chokkin::internals::PluginHints {
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
    assert_eq!(django.entries, []);
    assert_eq!(hints.warnings, []);
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

/// A project with `deps` declared and `files` written, for the plugins
/// that read definitions from config files (#401).
fn orchestration_hints(
    deps: &str,
    tables: &str,
    files: &[(&str, &str)],
) -> chokkin::internals::PluginHints {
    let plugin = deps.trim_matches('"').trim_start_matches("apache-");
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path();
    std::fs::write(
        root.join("pyproject.toml"),
        format!(
            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [{deps}]\n[tool.chokkin.plugins]\n{plugin} = true\n{tables}"
        ),
    )
    .expect("write pyproject");
    for (file, text) in files {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, text).expect("write fixture");
    }
    extract_at(root)
}

fn sorted_entries(contrib: &chokkin::internals::PluginContribution) -> Vec<String> {
    let mut paths: Vec<String> = entry_paths(contrib)
        .into_iter()
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths
}

/// The values a plugin did not read, which cap what they could hide.
fn gap_texts(contrib: &chokkin::internals::PluginContribution) -> Vec<String> {
    contrib
        .command_targets
        .iter()
        .map(|target| {
            let value = serde_json::to_value(&target.target).expect("serialize target");
            value["Opaque"]["text"]
                .as_str()
                .expect("an opaque target")
                .to_owned()
        })
        .collect()
}

#[test]
fn airflow_safe_mode_loads_dag_looking_files_only() {
    let dag = "from airflow import DAG\n";
    let files = |safe_mode: &str| {
        let cfg =
            format!("[core]\ndags_folder = pipelines\ndag_discovery_safe_mode = {safe_mode}\n");
        let hints = orchestration_hints(
            "\"apache-airflow\"",
            "",
            &[
                ("airflow.cfg", cfg.as_str()),
                ("pipelines/etl.py", dag),
                ("pipelines/util.py", "def helper():\n    pass\n"),
                ("dags/sub/load.py", "from airflow.sdk import dag\n"),
                ("tests/dags_broken/x.py", "import os\n"),
            ],
        );
        sorted_entries(plugin_contrib(&hints, PluginId::Airflow))
    };
    assert_eq!(files("True"), ["dags/sub/load.py", "pipelines/etl.py"]);
    assert_eq!(
        files("False"),
        ["dags/sub/load.py", "pipelines/etl.py", "pipelines/util.py"]
    );
}

/// Every `.py` under a directory named exactly `dags` is a DAG-folder file
/// in its path's context, whether or not it passes the safe-mode check
/// (#1064): tests and e2e suites hand the folder or the file to the DAG
/// processor by path. The partial names keep the check.
#[test]
fn airflow_dags_directories_load_every_file() {
    let license =
        "# Licensed to the Apache Software Foundation (ASF)\nfrom __future__ import annotations\n";
    let dag = "from airflow.sdk import dag\n";
    let hints = orchestration_hints(
        "\"apache-airflow\"",
        "",
        &[
            ("airflow.cfg", "[core]\ndags_folder = pipelines\n"),
            ("pipelines/util.py", "def helper():\n    pass\n"),
            ("dags/__init__.py", ""),
            ("dags/common/helper.py", "def run():\n    pass\n"),
            ("tests/__init__.py", ""),
            ("tests/unit/__init__.py", ""),
            ("tests/unit/dags/no_dags.py", license),
            (
                "tests/unit/dags/subdir2/subdir3/should_ignore_this.py",
                license,
            ),
            ("tests/integration/otel/dags/otel_test_dag.py", dag),
            ("tests/dags_broken/x.py", "import os\n"),
            ("tests/Dags/x.py", "import os\n"),
        ],
    );
    let contrib = plugin_contrib(&hints, PluginId::Airflow);
    let mut entries: Vec<(&str, FileContext)> = contrib
        .entries
        .iter()
        .map(|entry| (entry.spec.path.as_str(), entry.context))
        .collect();
    entries.sort_unstable_by_key(|(path, _)| *path);
    assert_eq!(
        entries,
        [
            ("dags/__init__.py", FileContext::Runtime),
            ("dags/common/helper.py", FileContext::Runtime),
            (
                "tests/integration/otel/dags/otel_test_dag.py",
                FileContext::Test
            ),
            ("tests/unit/dags/no_dags.py", FileContext::Test),
            (
                "tests/unit/dags/subdir2/subdir3/should_ignore_this.py",
                FileContext::Test
            ),
        ]
    );
}

#[test]
fn airflow_unreadable_dags_folder_is_a_gap() {
    let hints = orchestration_hints(
        "\"apache-airflow\"",
        "",
        &[("airflow.cfg", "[core]\ndags_folder = ${DAGS}\n")],
    );
    let contrib = plugin_contrib(&hints, PluginId::Airflow);
    assert!(contrib.entries.is_empty());
    assert_eq!(gap_texts(contrib), ["${DAGS}"]);
    assert_eq!(contrib.command_targets[0].origin.file, "airflow.cfg");
    assert_eq!(contrib.command_targets[0].origin.line, Some(2));
}

/// Airflow 3's `[dag_processor] dag_bundle_config_list` (#1058): a
/// `LocalDagBundle` `path` is a DAG folder under the same safe mode, and
/// what names DAGs elsewhere or does not read is a gap at the key's line.
#[test]
fn airflow_local_dag_bundles_are_dag_folders() {
    let cfg = "[core]\ndag_discovery_safe_mode = True\n\n[dag_processor]\ndag_bundle_config_list = [\n    {\n      \"name\": \"etl\",\n      \"classpath\": \"airflow.dag_processing.bundles.local.LocalDagBundle\",\n      \"kwargs\": {\"path\": \"{AIRFLOW_HOME}/pipelines\", \"refresh_interval\": 0}\n    },\n    {\"name\": \"default\", \"classpath\": \"airflow.dag_processing.bundles.local.LocalDagBundle\", \"kwargs\": {}},\n    {\"name\": \"abs\", \"classpath\": \"airflow.dag_processing.bundles.local.LocalDagBundle\", \"kwargs\": {\"path\": \"/opt/airflow/dags\"}},\n    {\"name\": \"git\", \"classpath\": \"airflow.providers.git.bundles.git.GitDagBundle\", \"kwargs\": {\"subdir\": \"dags\"}}\n  ]\n";
    let hints = orchestration_hints(
        "\"apache-airflow\"",
        "",
        &[
            ("airflow.cfg", cfg),
            ("pipelines/etl.py", "from airflow.sdk import dag\n"),
            ("pipelines/util.py", "def helper():\n    pass\n"),
        ],
    );
    let contrib = plugin_contrib(&hints, PluginId::Airflow);
    assert_eq!(sorted_entries(contrib), ["pipelines/etl.py"]);
    assert_eq!(
        contrib.entries[0].origin.label, "dag_bundle_config_list",
        "{:?}",
        contrib.entries[0].origin
    );
    assert_eq!(
        gap_texts(contrib),
        [
            "/opt/airflow/dags",
            "airflow.providers.git.bundles.git.GitDagBundle"
        ]
    );
    for target in &contrib.command_targets {
        assert_eq!(target.origin.file, "airflow.cfg");
        assert_eq!(target.origin.line, Some(5));
    }
}

#[test]
fn airflow_unreadable_dag_bundle_list_is_a_gap() {
    let hints = orchestration_hints(
        "\"apache-airflow\"",
        "",
        &[(
            "airflow.cfg",
            "[dag_processor]\ndag_bundle_config_list = pipelines\n",
        )],
    );
    let contrib = plugin_contrib(&hints, PluginId::Airflow);
    assert!(contrib.entries.is_empty());
    assert_eq!(gap_texts(contrib), ["pipelines"]);
    assert_eq!(contrib.command_targets[0].origin.line, Some(2));
}

#[test]
fn dagster_reads_code_locations_from_pyproject_dg_and_workspace() {
    let hints = orchestration_hints(
        "\"dagster\"",
        "",
        &[
            ("dg.toml", "[project]\nroot_module = \"acme_dg\"\n"),
            ("acme_dg/__init__.py", ""),
            (
                "acme_dg/definitions.py",
                "import dagster as dg\n\ndefs = dg.Definitions()\n",
            ),
            ("acme_dg/defs/__init__.py", ""),
            ("acme_dg/defs/assets.py", "x = 1\n"),
            (
                "workspace.yaml",
                concat!(
                    "load_from:\n",
                    "  # a comment\n",
                    "  - python_file:\n",
                    "      relative_path: jobs/repo.py\n",
                    "      location_name: jobs\n",
                    "  - python_file: jobs/missing.py\n",
                    "  - python_package: acme_pkg\n",
                    "  - python_module: external.defs\n",
                    "  - grpc_server:\n",
                    "      host: localhost\n",
                    "  - python_file: \"{{ env.FILE }}.py\"\n",
                    "  - python_entrypoint: acme.main\n",
                ),
            ),
            ("jobs/repo.py", "from dagster import repository\n"),
            ("acme_pkg/__init__.py", ""),
        ],
    );
    let contrib = plugin_contrib(&hints, PluginId::Dagster);
    assert_eq!(
        sorted_entries(contrib),
        [
            "acme_dg/definitions.py",
            "acme_dg/defs/__init__.py",
            "acme_dg/defs/assets.py",
            "acme_pkg/__init__.py",
            "jobs/repo.py",
        ]
    );
    assert_eq!(
        contrib
            .module_refs
            .iter()
            .map(|reference| reference.module.as_str())
            .collect::<Vec<_>>(),
        ["external.defs"]
    );
    assert_eq!(
        gap_texts(contrib),
        ["{{ env.FILE }}.py", "python_entrypoint: acme.main",]
    );
}

#[test]
fn dagster_tool_table_reads_module_names_and_reports_others() {
    let hints = orchestration_hints(
        "\"dagster\"",
        "\n[tool.dagster]\nmodule_name = [\"acme_a.defs\", \"not a module\"]\n",
        &[("acme_a/__init__.py", ""), ("acme_a/defs.py", "x = 1\n")],
    );
    let contrib = plugin_contrib(&hints, PluginId::Dagster);
    assert_eq!(sorted_entries(contrib), ["acme_a/defs.py"]);
    assert_eq!(gap_texts(contrib), ["not a module"]);
    assert_eq!(contrib.command_targets[0].origin.line, Some(9));
}

/// A module binding a dagster definitions name, imported or read off an
/// imported `dagster`, is an entry; other dagster names are not.
#[test]
fn dagster_definitions_modules_need_a_definitions_name() {
    let hints = orchestration_hints(
        "\"dagster\"",
        "",
        &[
            ("acme/__init__.py", ""),
            (
                "acme/a.py",
                "import dagster as dg\n\ndefs = dg.Definitions()\n",
            ),
            ("acme/b.py", "from dagster import asset\n"),
            (
                "acme/c.py",
                "import other as dg\n\ndefs = dg.Definitions()\n",
            ),
            ("acme/d.py", "import dagster as dg\n\nx = dg.asset\n"),
        ],
    );
    let contrib = plugin_contrib(&hints, PluginId::Dagster);
    assert_eq!(sorted_entries(contrib), ["acme/a.py"]);
    assert_eq!(contrib.entries[0].origin.line, Some(3));
}

fn module_ref_names(contrib: &chokkin::internals::PluginContribution) -> Vec<&str> {
    contrib
        .module_refs
        .iter()
        .map(|reference| reference.module.as_str())
        .collect()
}

/// An explicit `dg` setting is read whether or not its file exists, and a
/// value that is no dotted name is a gap.
#[test]
fn dagster_explicit_dg_settings_are_read_or_reported() {
    let hints = orchestration_hints(
        "\"dagster\"",
        "\n[tool.dg.project]\ndefs_module = \"not valid\"\ncode_location_target_module = \"bad name\"\n",
        &[(
            "dg.toml",
            "[project]\ncode_location_target_module = \"external.defs\"\n",
        )],
    );
    let contrib = plugin_contrib(&hints, PluginId::Dagster);
    assert!(contrib.entries.is_empty());
    assert_eq!(module_ref_names(contrib), ["external.defs"]);
    assert_eq!(gap_texts(contrib), ["\"not valid\"", "\"bad name\""]);
}

/// The modules a `root_module` implies are read only when they are dotted
/// names with a file; a file building `Definitions` keeps its own context.
#[test]
fn dagster_implied_dg_settings_need_a_file() {
    let hints = orchestration_hints(
        "\"dagster\"",
        "\n[tool.dg.project]\nroot_module = \"bad name\"\n",
        &[
            ("dg.toml", "[project]\nroot_module = \"acme_dg\"\n"),
            ("acme_dg/__init__.py", ""),
            ("acme_dg/definitions.py", "x = 1\n"),
            ("tests/test_defs.py", "from dagster import Definitions\n"),
        ],
    );
    let contrib = plugin_contrib(&hints, PluginId::Dagster);
    assert_eq!(
        sorted_entries(contrib),
        ["acme_dg/definitions.py", "tests/test_defs.py"]
    );
    let test_entry = contrib
        .entries
        .iter()
        .find(|entry| entry.spec.path == "tests/test_defs.py")
        .expect("test entry");
    assert_eq!(test_entry.context, FileContext::Test);
    assert!(module_ref_names(contrib).is_empty());
    assert!(gap_texts(contrib).is_empty());
}

/// `load_from` items at the key's own indent, blank lines inside and
/// between them, and the key that ends the list; a line that is no list
/// item is a gap.
#[test]
fn dagster_load_from_reads_the_list_to_its_end() {
    let hints = orchestration_hints(
        "\"dagster\"",
        "",
        &[
            (
                "workspace.yaml",
                concat!(
                    "load_from:\n",
                    "  stray: x\n",
                    "- python_module: col_zero.defs\n",
                    "\n",
                    "- python_file:\n",
                    "\n",
                    "    relative_path: jobs/spaced.py\n",
                    "- python_file:\n",
                    "other:\n",
                    "- python_module: not_loaded.defs\n",
                ),
            ),
            ("jobs/spaced.py", "x = 1\n"),
        ],
    );
    let contrib = plugin_contrib(&hints, PluginId::Dagster);
    assert_eq!(sorted_entries(contrib), ["jobs/spaced.py"]);
    assert_eq!(module_ref_names(contrib), ["col_zero.defs"]);
    assert_eq!(gap_texts(contrib), ["stray: x", "python_file:"]);
    assert_eq!(contrib.command_targets[0].origin.line, Some(2));
}

#[test]
fn prefect_entrypoints_root_files_and_report_templates() {
    let hints = orchestration_hints(
        "\"prefect\"",
        "",
        &[
            (
                "prefect.yaml",
                concat!(
                    "pull:\n",
                    "  - prefect.deployments.steps.set_working_directory:\n",
                    "      directory: /opt/flows\n",
                    "deployments:\n",
                    "  - name: etl\n",
                    "    entrypoint: flows/etl.py:etl_flow\n",
                    "  - name: dotted\n",
                    "    entrypoint: acme.flows.nightly\n",
                    "  - name: unset\n",
                    "    entrypoint: null\n",
                    "  - name: gone\n",
                    "    entrypoint: flows/gone.py:run\n",
                    "  - name: templated\n",
                    "    entrypoint: \"{{ $ENTRY }}\"\n",
                ),
            ),
            ("flows/etl.py", "from prefect import flow\n"),
        ],
    );
    let contrib = plugin_contrib(&hints, PluginId::Prefect);
    assert_eq!(sorted_entries(contrib), ["flows/etl.py"]);
    assert_eq!(contrib.entries[0].spec.symbol.as_deref(), Some("etl_flow"));
    assert_eq!(
        contrib
            .symbol_refs
            .iter()
            .map(|reference| (reference.module.as_str(), reference.symbol.as_str()))
            .collect::<Vec<_>>(),
        [("acme.flows", "nightly")]
    );
    assert_eq!(gap_texts(contrib), ["{{ $ENTRY }}"]);
    assert_eq!(contrib.command_targets[0].origin.line, Some(14));
}

#[test]
fn scrapy_settings_root_spiders_and_named_components() {
    let hints = orchestration_hints(
        "\"scrapy\"",
        "",
        &[
            (
                "scrapy.cfg",
                "[settings]\ndefault = crawler.settings\n\n[deploy]\nurl = http://localhost:6800/\n",
            ),
            ("crawler/__init__.py", ""),
            (
                "crawler/settings.py",
                concat!(
                    "BOT_NAME = \"crawler\"\n",
                    "SPIDER_MODULES = (\"crawler.spiders\",)\n",
                    "NEWSPIDER_MODULE = \"crawler.spiders\"\n",
                    "ITEM_PIPELINES = {\"crawler.pipelines.SavePipeline\": 300}\n",
                    "DOWNLOADER_MIDDLEWARES = {\"scrapy.downloadermiddlewares.useragent.UserAgentMiddleware\": None}\n",
                    "DUPEFILTER_CLASS = \"crawler.dupes\"\n",
                    "FEED_EXPORTERS_LIST = [\"crawler.ext_a\"]\n",
                    "SPIDER_LOADERS = (\"crawler.ext_b\",)\n",
                ),
            ),
            ("crawler/ext_a.py", "x = 1\n"),
            ("crawler/ext_b.py", "x = 1\n"),
            ("crawler/spiders/__init__.py", ""),
            ("crawler/spiders/quotes.py", "import scrapy\n"),
            ("crawler/spiders/nested/__init__.py", ""),
            ("crawler/spiders/nested/books.py", "import scrapy\n"),
            ("crawler/pipelines.py", "class SavePipeline:\n    pass\n"),
            ("crawler/dupes.py", "x = 1\n"),
        ],
    );
    let contrib = plugin_contrib(&hints, PluginId::Scrapy);
    assert_eq!(
        sorted_entries(contrib),
        [
            "crawler/dupes.py",
            "crawler/ext_a.py",
            "crawler/ext_b.py",
            "crawler/pipelines.py",
            "crawler/settings.py",
            "crawler/spiders/__init__.py",
            "crawler/spiders/nested/__init__.py",
            "crawler/spiders/nested/books.py",
            "crawler/spiders/quotes.py",
        ]
    );
    assert_eq!(
        contrib
            .symbol_refs
            .iter()
            .map(|reference| (reference.module.as_str(), reference.symbol.as_str()))
            .collect::<Vec<_>>(),
        [("crawler.pipelines", "SavePipeline")]
    );
    assert!(gap_texts(contrib).is_empty());
}

#[test]
fn scrapy_unread_spider_modules_and_settings_are_gaps() {
    let hints = orchestration_hints(
        "\"scrapy\"",
        "",
        &[
            (
                "scrapy.cfg",
                "[settings]\ndefault = crawler.settings\nother = ${SETTINGS}\nexternal = vendor.settings\n",
            ),
            ("crawler/__init__.py", ""),
            (
                "crawler/settings.py",
                "import os\nSPIDER_MODULES = [\"crawler.spiders\", os.environ[\"MORE\"]]\n",
            ),
            ("crawler/spiders/__init__.py", ""),
        ],
    );
    let contrib = plugin_contrib(&hints, PluginId::Scrapy);
    assert_eq!(sorted_entries(contrib), ["crawler/settings.py"]);
    assert_eq!(
        contrib
            .module_refs
            .iter()
            .map(|reference| reference.module.as_str())
            .collect::<Vec<_>>(),
        ["vendor.settings"]
    );
    let mut gaps = gap_texts(contrib);
    gaps.sort();
    assert_eq!(
        gaps,
        ["${SETTINGS}", "[\"crawler.spiders\", os.environ[\"MORE\"]]",]
    );
}
