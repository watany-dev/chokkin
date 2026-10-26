//! Integration tests for entry root construction (pipeline step 8).

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::path::{Path, PathBuf};

use chokkin::internals::{
    EntryOrigin, EntryWarning, FileContext, PluginExtractRequest, ProjectMode, ProjectRoot,
    RootMarker, build_entry_roots, discover_project_root, discover_sources, extract_manifest,
    extract_plugin_hints_with_parse, load_config, parse_project_sources_with_cache,
    resolve_target_version,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/entry")
        .join(name)
}

fn plugins_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plugins")
        .join(name)
}

fn sources_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sources")
        .join(name)
}

fn project_root_at(path: &Path) -> ProjectRoot {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    ProjectRoot {
        path: canonical,
        marker: RootMarker::PyProjectToml,
    }
}

struct PipelineInputs {
    config: chokkin::internals::ChokkinConfig,
    manifest: chokkin::internals::LoadedManifest,
    sources: chokkin::internals::DiscoveredSources,
    plugins: chokkin::internals::PluginHints,
}

fn load_pipeline(path: &Path) -> PipelineInputs {
    let root = discover_project_root(path).unwrap_or_else(|_| project_root_at(path));
    let loaded = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &loaded).expect("extract manifest");
    let sources = discover_sources(&root, &loaded, &manifest).expect("discover sources");
    let target = resolve_target_version(&loaded.effective, &manifest);
    let parse = parse_project_sources_with_cache(&root, &sources, &target, None).expect("parse");
    let plugins = extract_plugin_hints_with_parse(&PluginExtractRequest {
        root: &root,
        config: &loaded,
        sources: &sources,
        manifest: &manifest,
        parse: &parse,
        cache: None,
        command_files: &[],
    })
    .expect("plugin hints");
    let config = loaded.effective;
    PipelineInputs {
        config,
        manifest,
        sources,
        plugins,
    }
}

fn entry_paths(plan: &chokkin::internals::EntryPlan) -> Vec<&str> {
    plan.roots.iter().map(|root| root.path.as_str()).collect()
}

#[test]
fn django_manage_is_entry_with_plugin_origins() {
    let inputs = load_pipeline(&plugins_fixture("django_manage"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    assert_eq!(plan.mode, ProjectMode::App);
    let manage = plan
        .roots
        .iter()
        .find(|root| root.path == "manage.py")
        .expect("manage.py entry");
    assert!(
        manage
            .origins
            .iter()
            .any(|origin| matches!(origin, EntryOrigin::Plugin { .. }))
    );
}

#[test]
fn fastapi_asgi_is_auto_detected() {
    let inputs = load_pipeline(&fixture("fastapi_asgi"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    let paths = entry_paths(&plan);
    assert!(paths.iter().any(|path| path.contains("asgi.py")));
    // `src/acme/asgi.py` is a module of the named package, like werkzeug's
    // `wsgi.py`, so it stays an entry but no longer makes an app (#652).
    assert_eq!(plan.mode, ProjectMode::Library);
}

#[test]
fn library_only_resolves_library_mode() {
    let inputs = load_pipeline(&fixture("library_only"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    assert_eq!(plan.mode, ProjectMode::Library);
}

#[test]
fn setup_py_without_static_name_resolves_library_mode() {
    let inputs = load_pipeline(&fixture("setup_py_dynamic_name"));
    assert_eq!(inputs.manifest.metadata.name, None);
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    assert_eq!(plan.mode, ProjectMode::Library);
}

/// textual names its distribution only under `[tool.poetry]` (#974).
#[test]
fn poetry_only_name_resolves_library_mode() {
    let inputs = load_pipeline(&fixture("poetry_only_library"));
    assert_eq!(inputs.manifest.metadata.name.as_deref(), Some("acme"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    assert_eq!(plan.mode, ProjectMode::Library);
}

#[test]
fn explicit_config_entry_merges_with_auto() {
    let inputs = load_pipeline(&fixture("explicit_entry"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    let manage = plan
        .roots
        .iter()
        .find(|root| root.path == "manage.py")
        .expect("manage.py");
    assert!(manage.origins.contains(&EntryOrigin::Config));
    assert!(
        manage
            .origins
            .iter()
            .any(|origin| matches!(origin, EntryOrigin::Auto { .. }))
    );
}

#[test]
fn missing_config_entry_emits_warning() {
    let inputs = load_pipeline(&sources_fixture("missing_entry"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    assert!(
        plan.warnings
            .iter()
            .any(|warning| matches!(warning, EntryWarning::MissingEntryPath { .. }))
    );
    assert!(!plan.roots.iter().any(|root| root.path == "missing.py"));
}

#[test]
fn production_excludes_test_context_entries() {
    let inputs = load_pipeline(&plugins_fixture("pytest_pyproject"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        true,
    );

    assert!(
        plan.roots
            .iter()
            .all(|root| root.context.is_included_in_production())
    );
}

#[test]
fn manifest_script_resolves_to_module_file() {
    let inputs = load_pipeline(&plugins_fixture("fastapi_scripts"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    let script_entry = plan.roots.iter().find(|root| {
        root.origins.iter().any(|origin| {
            matches!(
                origin,
                EntryOrigin::Manifest { name, .. } if name == "start"
            ) || matches!(
                origin,
                EntryOrigin::SymbolRef { label, .. } if label.contains("project.scripts.start")
            )
        })
    });
    assert!(script_entry.is_some());
    let entry = script_entry.expect("uvicorn script entry");
    assert_eq!(entry.path, "src/pkg/main.py");
    assert_eq!(
        entry.symbols.iter().map(String::as_str).collect::<Vec<_>>(),
        ["app"]
    );
}

/// #802: only the package's own `__init__.py` makes a subpackage, not a
/// stray `src/__init__.py` above it.
#[test]
fn library_roots_mark_subpackages_below_the_package_init() {
    let inputs = load_pipeline(&fixture("library_subpackage"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    assert_eq!(plan.mode, ProjectMode::Library);
    let origins_of = |path: &str| {
        let root = plan
            .roots
            .iter()
            .find(|root| root.path == path)
            .unwrap_or_else(|| panic!("{path} root"));
        root.origins.clone()
    };
    let auto = |rule: &str| {
        vec![EntryOrigin::Auto {
            rule: rule.to_owned(),
        }]
    };
    assert_eq!(origins_of("src/acme/__init__.py"), auto("library package"));
    assert_eq!(
        origins_of("src/acme/sub/__init__.py"),
        auto("library subpackage")
    );
}

#[test]
fn commands_running_scripts_by_path_are_dev_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let write = |rel: &str, contents: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    };
    write(
        "pyproject.toml",
        "[project]\nname = \"acme\"\nversion = \"0\"\n\n[tool.chokkin]\nproject = [\"**/*.py\"]\n",
    );
    write("acme/__init__.py", "");
    write("tools/harness.py", "");
    write("task-sdk/dev/gen.py", "");
    write(".agents/skills/release/scripts/bump.py", "");
    write("tools/orphan.py", "");
    write(
        ".github/workflows/ci.yml",
        "jobs:\n  t:\n    steps:\n      - uses: actions/checkout@v4\n        with:\n          path: repo\n      - run: python3 repo/tools/harness.py out.xml\n      - run: python ${{ matrix.tool }}/orphan.py\n",
    );
    write(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: local\n    hooks:\n      - id: gen\n        entry: uv run -p 3.12 --directory task-sdk -s dev/gen.py\n        language: python\n",
    );
    write(
        ".agents/skills/release/SKILL.md",
        "```bash\n   uv run .agents/skills/release/scripts/bump.py --check\n```\n",
    );

    let inputs = load_pipeline(root);
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    let command_roots: Vec<_> = plan
        .roots
        .iter()
        .filter(|root| {
            root.origins
                .iter()
                .any(|origin| matches!(origin, EntryOrigin::Command { .. }))
        })
        .map(|root| (root.path.as_str(), root.context))
        .collect();
    assert_eq!(
        command_roots,
        [
            (".agents/skills/release/scripts/bump.py", FileContext::Dev),
            ("task-sdk/dev/gen.py", FileContext::Dev),
            ("tools/harness.py", FileContext::Dev),
        ]
    );
}

#[test]
fn servers_given_a_project_app_make_it_an_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let write = |rel: &str, contents: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    };
    write(
        "pyproject.toml",
        concat!(
            "[project]\nname = \"acme\"\nversion = \"0\"\n\n",
            "[tool.poe.tasks]\n",
            "start.help = \"Run\"\n",
            "start.cmd = \"granian --interface asgi acme.poe_app:app\"\n\n",
            "[tool.hatch.envs.default.scripts]\n",
            "serve = [\"hypercorn acme.hatch_app\"]\n",
        ),
    );
    for module in "__init__ asgi wsgi poe_app hatch_app cfg_app run_app".split(' ') {
        write(&format!("acme/{module}.py"), "");
    }
    write(
        "acme/__main__.py",
        "import uvicorn\nconfig = uvicorn.Config(\"acme.cfg_app:application\", port=8000)\n",
    );
    write(
        "acme/serve.py",
        "from uvicorn import run\n\ndef main():\n    run(app=\"acme.run_app:app\")\n",
    );
    write(
        "Dockerfile",
        "FROM python\nCMD [\"uvicorn\", \"acme.asgi:application\", \"--host=0.0.0.0\"]\n",
    );
    write(
        "Procfile",
        "web: gunicorn acme.wsgi\nother: uvicorn elsewhere.app:app\n",
    );

    let inputs = load_pipeline(root);
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        true,
    );

    let server_roots: Vec<_> = plan
        .roots
        .iter()
        .filter_map(|root| {
            root.origins.iter().find_map(|origin| match origin {
                EntryOrigin::Command { label } => Some(format!(
                    "{} {:?} {:?} {label}",
                    root.path, root.symbols, root.context
                )),
                _ => None,
            })
        })
        .collect();
    assert_eq!(
        server_roots,
        [
            r#"acme/asgi.py {"application"} Runtime CMD (Dockerfile:2)"#,
            r#"acme/cfg_app.py {"application"} Runtime uvicorn.Config (acme/__main__.py:2)"#,
            r#"acme/hatch_app.py {"app"} Runtime tool.hatch.envs.default.scripts.serve (pyproject.toml:10)"#,
            r#"acme/poe_app.py {"app"} Runtime tool.poe.tasks.start (pyproject.toml:6)"#,
            r#"acme/run_app.py {"app"} Runtime uvicorn.run (acme/serve.py:4)"#,
            r#"acme/wsgi.py {"application"} Runtime process web (Procfile:1)"#,
        ]
    );
    // An app outside the project is no entry and no warning.
    assert!(
        plan.warnings.is_empty(),
        "unexpected warnings: {:?}",
        plan.warnings
    );
}

/// `python acme/standalone.py` finds `sibling` next to the script, an
/// `app_dir` app names no known file, and an excluded app file warns.
#[test]
fn server_apps_beside_scripts_moved_or_excluded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let write = |rel: &str, contents: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    };
    write(
        "pyproject.toml",
        "[project]\nname = \"acme\"\nversion = \"0\"\n\n[tool.chokkin]\nexclude = [\"acme/asgi.py\"]\n",
    );
    write("acme/__init__.py", "");
    write("acme/asgi.py", "");
    write("acme/sibling.py", "");
    write("acme/dir_app.py", "");
    write(
        "acme/standalone.py",
        "import uvicorn\nuvicorn.run(\"sibling:app\")\nuvicorn.run(\"acme.dir_app:app\", app_dir=\"x\")\n",
    );
    write("Procfile", "web: uvicorn acme.asgi:app\n");

    let inputs = load_pipeline(root);
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &[],
        &inputs.plugins,
        false,
    );

    assert_eq!(
        plan.warnings,
        [EntryWarning::MissingEntryPath {
            path: "acme/asgi.py".to_owned()
        }]
    );
    let server_roots: Vec<_> = plan
        .roots
        .iter()
        .filter(|root| {
            root.origins
                .iter()
                .any(|origin| matches!(origin, EntryOrigin::Command { .. }))
        })
        .map(|root| root.path.as_str())
        .collect();
    assert_eq!(server_roots, ["acme/sibling.py"]);
}

#[test]
fn workspace_members_add_entry_roots_and_library_members() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let write = |rel: &str, contents: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    };
    write(
        "pyproject.toml",
        "[tool.uv.workspace]\nmembers = [\"libs/core\", \"apps/cli\"]\n",
    );
    write(
        "libs/core/pyproject.toml",
        "[project]\nname = \"core\"\nversion = \"0\"\n",
    );
    write("libs/core/src/core/__init__.py", "");
    write(
        "apps/cli/pyproject.toml",
        "[project]\nname = \"cli\"\nversion = \"0\"\n\n[project.scripts]\ncli = \"cli.main:run\"\n",
    );
    write("apps/cli/src/cli/__init__.py", "");
    write("apps/cli/src/cli/main.py", "def run():\n    pass\n");

    let inputs = load_pipeline(root);
    let core = load_pipeline(&root.join("libs/core"));
    let cli = load_pipeline(&root.join("apps/cli"));
    let members = [
        ("libs/core", &core.manifest, &core.sources),
        ("apps/cli", &cli.manifest, &cli.sources),
    ];
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &members,
        &inputs.plugins,
        false,
    );

    assert_eq!(plan.library_members, ["libs/core"]);
    let script = plan
        .roots
        .iter()
        .find(|root| root.path == "apps/cli/src/cli/main.py")
        .unwrap_or_else(|| panic!("{:#?}", entry_paths(&plan)));
    assert_eq!(script.symbols.iter().collect::<Vec<_>>(), ["run"]);
    assert!(
        matches!(script.origins.as_slice(), [EntryOrigin::Manifest { .. }]),
        "{:#?}",
        script.origins
    );
}
