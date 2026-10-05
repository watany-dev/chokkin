//! Integration tests for import resolution (pipeline step 7).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chokkin::resolver::StdlibRange;
use chokkin::{
    ModuleOrigin, PluginExtractRequest, ProjectRoot, ResolveConfidence, RootMarker,
    discover_project_root, discover_sources, extract_manifest, extract_plugin_hints_with_parse,
    load_config, parse_project_sources_with_cache, resolve_imports, resolve_target_version,
};

fn resolver_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/resolver")
        .join(name)
}

fn resolve_fixture(name: &str) -> chokkin::ResolutionIndex {
    resolve_path(&resolver_fixture(name))
}

fn resolve_path(path: &Path) -> chokkin::ResolutionIndex {
    let root = discover_project_root(path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        marker: RootMarker::PyProjectToml,
    });
    let loaded = load_config(&root).expect("config");
    let manifest = extract_manifest(&root, &loaded).expect("manifest");
    let sources = discover_sources(&root, &loaded, &manifest).expect("sources");
    let target = resolve_target_version(&loaded.effective, &manifest);
    let parse = parse_project_sources_with_cache(&root, &sources, &target, None).expect("parse");
    let plugins = extract_plugin_hints_with_parse(&PluginExtractRequest {
        root: &root,
        config: &loaded,
        sources: &sources,
        manifest: &manifest,
        parse: &parse,
        cache: None,
    })
    .expect("plugins");
    let plugin_refs: Vec<_> = plugins.module_refs().cloned().collect();
    resolve_imports(
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &plugin_refs,
        &loaded.workspace_members,
    )
}

#[test]
fn resolves_stdlib_import() {
    let index = resolve_fixture("stdlib");
    assert!(index.imports.iter().any(|resolved| {
        resolved.full_module == "os" && resolved.origin == ModuleOrigin::Stdlib
    }));
}

#[test]
fn resolves_first_party_import() {
    let index = resolve_fixture("first_party");
    assert!(index.imports.iter().any(|resolved| {
        resolved.import_root == "acme" && resolved.origin == ModuleOrigin::FirstParty
    }));
}

#[test]
fn resolves_workspace_member_import_from_resolved_member_id() {
    let index = resolve_fixture("uv_workspace_member");
    assert!(index.imports.iter().any(|resolved| {
        resolved.import_root == "api"
            && resolved.origin == ModuleOrigin::FirstParty
            && resolved.workspace_member.is_none()
    }));
    assert!(index.imports.iter().any(|resolved| {
        resolved.import_root == "os" && resolved.workspace_member.as_deref() == Some("api")
    }));
}

#[test]
fn resolves_yaml_to_pyyaml() {
    let index = resolve_fixture("third_party");
    let yaml = index
        .imports
        .iter()
        .find(|resolved| resolved.import_root == "yaml")
        .expect("yaml import");
    assert_eq!(yaml.origin, ModuleOrigin::ThirdParty);
    assert_eq!(yaml.distribution.as_deref(), Some("pyyaml"));
    assert_eq!(yaml.confidence, ResolveConfidence::Certain);
}

#[test]
fn resolves_pil_to_pillow() {
    let index = resolve_fixture("pillow_import");
    let pil = index
        .imports
        .iter()
        .find(|resolved| resolved.import_root == "PIL")
        .expect("PIL import");
    assert_eq!(pil.distribution.as_deref(), Some("pillow"));
}

#[test]
fn user_package_module_map_overrides_bundled() {
    let index = resolve_fixture("user_map");
    let custom = index
        .imports
        .iter()
        .find(|resolved| resolved.import_root == "yaml")
        .expect("yaml");
    assert_eq!(custom.distribution.as_deref(), Some("custom-yaml"));
    assert_eq!(custom.confidence, ResolveConfidence::Likely);
}

#[test]
fn venv_metadata_takes_priority() {
    let index = resolve_fixture("venv_priority");
    let demo = index
        .imports
        .iter()
        .find(|resolved| resolved.import_root == "demo_pkg")
        .expect("demo_pkg");
    assert_eq!(demo.distribution.as_deref(), Some("demo-dist"));
}

#[test]
fn pep723_requires_python_sets_the_script_stdlib_target() {
    // Generated at test time: a checked-in copy would be analyzed by the
    // repository's own chokkin baseline run as a reachable script.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let block = |spec: &str| {
        format!("# /// script\n# requires-python = \"{spec}\"\n# ///\nimport tomllib\n")
    };
    std::fs::create_dir_all(temp.path().join("scripts")).expect("scripts dir");
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"pep723-target\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\n"
                .to_owned(),
        ),
        ("app.py", "import tomllib\n".to_owned()),
        ("scripts/new.py", block(">=3.11")),
        ("scripts/old.py", block(">=3.10,<3.11")),
    ] {
        std::fs::write(temp.path().join(file), text).expect("write fixture");
    }
    let root = discover_project_root(temp.path()).expect("root");
    let mut loaded = load_config(&root).expect("config");
    let manifest = extract_manifest(&root, &loaded).expect("manifest");
    let target = resolve_target_version(&loaded.effective, &manifest);
    assert_eq!(target.as_str(), "py310");
    loaded.effective.target_version = Some(target.clone());
    let sources = discover_sources(&root, &loaded, &manifest).expect("sources");
    let parse = parse_project_sources_with_cache(&root, &sources, &target, None).expect("parse");
    let (scripts, warnings) = chokkin::discover_inline_scripts(
        &root.path,
        sources.python_files().map(|file| file.path.as_str()),
    );
    assert_eq!(warnings, []);
    let script_targets: BTreeMap<_, _> = scripts
        .iter()
        .filter_map(|script| {
            let target = script.target_version.as_ref()?;
            let range = StdlibRange::new(target, script.requires_python.as_deref());
            Some((script.path.clone(), range))
        })
        .collect();
    let index = chokkin::resolver::resolve_imports_for_analysis(
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &[],
        &loaded.workspace_members,
        &script_targets,
        &chokkin::resolver::ScopedDeclarations::default(),
    );
    let tomllib_in = |file: &str| {
        index
            .imports
            .iter()
            .find(|resolved| resolved.file == file && resolved.import_root == "tomllib")
            .unwrap_or_else(|| panic!("tomllib import in {file}"))
    };

    assert_eq!(tomllib_in("scripts/new.py").origin, ModuleOrigin::Stdlib);
    assert_ne!(tomllib_in("scripts/old.py").origin, ModuleOrigin::Stdlib);
    // #358: `>=3.10` also covers 3.11+, where `tomllib` is stdlib.
    assert_eq!(tomllib_in("app.py").origin, ModuleOrigin::Stdlib);
}

/// Write a throwaway project: a checked-in copy would be analyzed by the
/// repository's own chokkin baseline run as unreachable files.
fn temp_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("tempdir");
    for (file, text) in files {
        let path = temp.path().join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture dir");
        }
        std::fs::write(path, text).expect("write fixture");
    }
    temp
}

#[test]
fn declared_name_resolves_an_unmapped_normalized_root() {
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"declared-unmapped-demo\"\nversion = \"0.1.0\"\ndependencies = [\"resolvelib>=1.0\"]\n",
        ),
        ("main.py", "import resolvelib\nimport notdeclaredanywhere\n"),
    ]);
    let index = resolve_path(temp.path());
    let origin = |root: &str| {
        index
            .imports
            .iter()
            .find(|resolved| resolved.import_root == root)
            .map(|resolved| (resolved.origin, resolved.distribution.clone()))
            .expect("import")
    };
    assert_eq!(
        origin("resolvelib"),
        (ModuleOrigin::ThirdParty, Some("resolvelib".to_owned()))
    );
    assert_eq!(origin("notdeclaredanywhere"), (ModuleOrigin::Unknown, None));
}

#[test]
fn dotted_map_entry_overrides_a_first_party_root_without_the_module() {
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"poetry\"\nversion = \"0.1.0\"\ndependencies = [\"poetry-core>=2.0\"]\n",
        ),
        ("src/poetry/__init__.py", ""),
        ("src/poetry/console/__init__.py", ""),
        (
            "src/poetry/app.py",
            "from poetry.console import main\nfrom poetry.core.version import Version\n",
        ),
    ]);
    let index = resolve_path(temp.path());
    let origin = |module: &str| {
        index
            .imports
            .iter()
            .find(|resolved| resolved.full_module == module)
            .map(|resolved| (resolved.origin, resolved.distribution.clone()))
            .expect("import")
    };
    assert_eq!(origin("poetry.console"), (ModuleOrigin::FirstParty, None));
    assert_eq!(
        origin("poetry.core.version"),
        (ModuleOrigin::ThirdParty, Some("poetry-core".to_owned()))
    );
}

#[test]
fn ambiguous_namespace_import_warns_once() {
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"app\"\nversion = \"0.1.0\"\n\n[tool.chokkin.package_module_map]\n\"acme-a\" = [\"acme.shared\"]\n\"acme-b\" = [\"acme.shared\"]\n",
        ),
        ("app/__init__.py", ""),
        ("app/one.py", "import acme.shared.x\n"),
        ("app/two.py", "import acme.shared.y\n"),
    ]);
    let index = resolve_path(temp.path());
    let ambiguous = index
        .warnings
        .iter()
        .filter(|warning| matches!(warning, chokkin::ResolveWarning::AmbiguousImport { .. }))
        .count();
    assert_eq!(ambiguous, 1, "{:?}", index.warnings);
}

#[test]
fn nested_fixture_does_not_count_as_the_local_namespace_module() {
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"poetry\"\nversion = \"0.1.0\"\ndependencies = [\"poetry-core>=2.0\"]\n",
        ),
        ("src/poetry/__init__.py", ""),
        (
            "src/poetry/app.py",
            "from poetry.core.version import Version\n",
        ),
        ("tests/fixtures/vendor/poetry/core/__init__.py", ""),
    ]);
    let index = resolve_path(temp.path());
    let resolved = index
        .imports
        .iter()
        .find(|resolved| resolved.full_module == "poetry.core.version")
        .expect("import");
    assert_eq!(
        (resolved.origin, resolved.distribution.as_deref()),
        (ModuleOrigin::ThirdParty, Some("poetry-core"))
    );
}

#[test]
fn local_module_keeps_a_first_party_namespace_root() {
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"protobuf\"\nversion = \"0.1.0\"\n",
        ),
        ("src/google/__init__.py", ""),
        ("src/google/protobuf/__init__.py", ""),
        (
            "src/google/protobuf/message.py",
            "from google.protobuf import descriptor\n",
        ),
    ]);
    let index = resolve_path(temp.path());
    let resolved = index
        .imports
        .iter()
        .find(|resolved| resolved.full_module == "google.protobuf")
        .expect("import");
    assert_eq!(resolved.origin, ModuleOrigin::FirstParty);
}

#[test]
fn declared_distribution_named_like_the_import_resolves_without_map_entry() {
    // `openai` is absent from the bundled map and already canonical, so only
    // the declaration can tell it apart from an unknown module.
    let temp = tempfile::TempDir::new().expect("tempdir");
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"declared-same-name\"\nversion = \"0.1.0\"\ndependencies = [\"openai\"]\n",
        ),
        ("app.py", "import openai\nimport notdeclaredpkg\n"),
    ] {
        std::fs::write(temp.path().join(file), text).expect("write fixture");
    }
    let index = resolve_path(temp.path());
    let root = |name: &str| {
        index
            .imports
            .iter()
            .find(|resolved| resolved.import_root == name)
            .unwrap_or_else(|| panic!("{name} import"))
    };

    assert_eq!(root("openai").origin, ModuleOrigin::ThirdParty);
    assert_eq!(root("openai").distribution.as_deref(), Some("openai"));
    assert_eq!(root("notdeclaredpkg").origin, ModuleOrigin::Unknown);
}

#[test]
fn normalized_root_resolves_only_through_a_declared_or_locked_name() {
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"normalized-demo\"\nversion = \"0.1.0\"\ndependencies = [\"Foo_Bar\"]\n",
        ),
        (
            "uv.lock",
            "version = 1\n\n[[package]]\nname = \"foo-bar\"\nversion = \"1.0\"\ndependencies = [{ name = \"locked-only\" }]\n\n[[package]]\nname = \"locked-only\"\nversion = \"1.0\"\n",
        ),
        (
            "app.py",
            "import foo_bar\nimport Locked_Only\nimport e2e_config\n",
        ),
    ]);
    let index = resolve_path(temp.path());
    let root = |name: &str| {
        index
            .imports
            .iter()
            .find(|resolved| resolved.import_root == name)
            .map_or_else(
                || panic!("{name} import"),
                |resolved| (resolved.origin, resolved.distribution.clone()),
            )
    };

    assert_eq!(
        root("foo_bar"),
        (ModuleOrigin::ThirdParty, Some("foo-bar".to_owned()))
    );
    assert_eq!(
        root("Locked_Only"),
        (ModuleOrigin::ThirdParty, Some("locked-only".to_owned()))
    );
    assert_eq!(root("e2e_config"), (ModuleOrigin::Unknown, None));
    assert!(index.warnings.iter().any(|warning| matches!(
        warning,
        chokkin::ResolveWarning::UnresolvedImport { import, .. } if import == "e2e_config"
    )));
}

#[test]
fn affixed_declared_name_resolves_as_maybe() {
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"affix-demo\"\nversion = \"0.1.0\"\ndependencies = [\"widget-py\", \"pygadget\"]\n",
        ),
        ("app.py", "import widget\nimport gadget\nimport gizmo\n"),
    ]);
    let index = resolve_path(temp.path());
    let root = |name: &str| {
        index
            .imports
            .iter()
            .find(|resolved| resolved.import_root == name)
            .map_or_else(
                || panic!("{name} import"),
                |resolved| {
                    (
                        resolved.origin,
                        resolved.distribution.clone(),
                        resolved.confidence,
                    )
                },
            )
    };

    assert_eq!(
        root("widget"),
        (
            ModuleOrigin::ThirdParty,
            Some("widget-py".to_owned()),
            ResolveConfidence::Maybe
        )
    );
    assert_eq!(root("gadget").1.as_deref(), Some("pygadget"));
    assert_eq!(root("gizmo").0, ModuleOrigin::Unknown);
}
