//! Integration tests for import resolution (pipeline step 7).

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chokkin::internals::StdlibRange;
use chokkin::internals::{
    LocalTrees, ModuleOrigin, PluginExtractRequest, ProjectRoot, ResolveConfidence, RootMarker,
    ScopedDeclarations, discover_project_root, discover_sources, extract_manifest,
    extract_plugin_hints_with_parse, load_config, parse_project_sources_with_cache,
    resolve_imports_for_analysis, resolve_target_version,
};

fn resolver_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/resolver")
        .join(name)
}

fn resolve_fixture(name: &str) -> chokkin::internals::ResolutionIndex {
    resolve_path(&resolver_fixture(name))
}

fn resolve_path(path: &Path) -> chokkin::internals::ResolutionIndex {
    resolve_path_scoped(path, &ScopedDeclarations::default())
}

fn resolve_path_scoped(
    path: &Path,
    scoped: &ScopedDeclarations,
) -> chokkin::internals::ResolutionIndex {
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
        dockerfiles: &[],
    })
    .expect("plugins");
    let plugin_refs: Vec<_> = plugins.module_refs().cloned().collect();
    resolve_imports_for_analysis(
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &plugin_refs,
        &[],
        &LocalTrees::new(&manifest, &loaded.workspace_members, &loaded.effective),
        &BTreeMap::new(),
        scoped,
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
        &*resolved.import_root == "acme" && resolved.origin == ModuleOrigin::FirstParty
    }));
}

#[test]
fn resolves_workspace_member_import_from_resolved_member_id() {
    let index = resolve_fixture("uv_workspace_member");
    assert!(index.imports.iter().any(|resolved| {
        &*resolved.import_root == "api"
            && resolved.origin == ModuleOrigin::FirstParty
            && resolved.workspace_member.is_none()
    }));
    assert!(index.imports.iter().any(|resolved| {
        &*resolved.import_root == "os" && resolved.workspace_member.as_deref() == Some("api")
    }));
}

#[test]
fn resolves_yaml_to_pyyaml() {
    let index = resolve_fixture("third_party");
    let yaml = index
        .imports
        .iter()
        .find(|resolved| &*resolved.import_root == "yaml")
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
        .find(|resolved| &*resolved.import_root == "PIL")
        .expect("PIL import");
    assert_eq!(pil.distribution.as_deref(), Some("pillow"));
}

#[test]
fn user_package_module_map_overrides_bundled() {
    let index = resolve_fixture("user_map");
    let custom = index
        .imports
        .iter()
        .find(|resolved| &*resolved.import_root == "yaml")
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
        .find(|resolved| &*resolved.import_root == "demo_pkg")
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
    let (scripts, warnings) = chokkin::internals::discover_inline_scripts(
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
    let index = chokkin::internals::resolve_imports_for_analysis(
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &[],
        &[],
        &LocalTrees::new(&manifest, &loaded.workspace_members, &loaded.effective),
        &script_targets,
        &chokkin::internals::ScopedDeclarations::default(),
    );
    let tomllib_in = |file: &str| {
        index
            .imports
            .iter()
            .find(|resolved| &*resolved.file == file && &*resolved.import_root == "tomllib")
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
            .find(|resolved| &*resolved.import_root == root)
            .map(|resolved| (resolved.origin, resolved.distribution.clone()))
            .expect("import")
    };
    assert_eq!(
        origin("resolvelib"),
        (ModuleOrigin::ThirdParty, Some("resolvelib".into()))
    );
    assert_eq!(origin("notdeclaredanywhere"), (ModuleOrigin::Unknown, None));
}

#[test]
fn each_site_picks_its_own_declared_candidate_for_a_shared_root() {
    // `pydantic-ai` and `pydantic-ai-slim` both provide `pydantic_ai` (#732).
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"shared-root\"\nversion = \"0.1.0\"\ndependencies = [\"pydantic-ai-slim\"]\n\n[tool.uv.workspace]\nmembers = [\"services/*\"]\n",
        ),
        ("main.py", "import pydantic_ai\n"),
        ("scripts/tool.py", "import pydantic_ai\n"),
        (
            "services/api/pyproject.toml",
            "[project]\nname = \"api\"\nversion = \"0.1.0\"\n",
        ),
        ("services/api/src/api/main.py", "import pydantic_ai\n"),
        (
            "services/web/pyproject.toml",
            "[project]\nname = \"web\"\nversion = \"0.1.0\"\n",
        ),
        ("services/web/src/web/main.py", "import pydantic_ai\n"),
    ]);
    let full = || BTreeSet::from(["pydantic-ai".to_owned()]);
    let scoped = ScopedDeclarations {
        scripts: BTreeMap::from([("scripts/tool.py".to_owned(), full())]),
        members: BTreeMap::from([("api".to_owned(), full())]),
        member_locks: BTreeMap::from([("web".to_owned(), full())]),
    };
    let index = resolve_path_scoped(temp.path(), &scoped);
    let distribution = |file: &str| {
        index
            .imports
            .iter()
            .find(|resolved| &*resolved.file == file)
            .and_then(|resolved| resolved.distribution.as_deref().map(str::to_owned))
            .expect("import")
    };
    assert_eq!(distribution("main.py"), "pydantic-ai-slim");
    assert_eq!(distribution("scripts/tool.py"), "pydantic-ai");
    assert_eq!(distribution("services/api/src/api/main.py"), "pydantic-ai");
    // The root's declaration beats a name the member only locks.
    assert_eq!(
        distribution("services/web/src/web/main.py"),
        "pydantic-ai-slim"
    );
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
        (ModuleOrigin::ThirdParty, Some("poetry-core".into()))
    );
}

#[test]
fn namespace_import_picks_the_declared_candidate() {
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"acme-b\"]\n\n[tool.chokkin.package_module_map]\n\"acme-a\" = [\"acme.shared\"]\n\"acme-b\" = [\"acme.shared\"]\n",
        ),
        ("app/__init__.py", ""),
        ("app/one.py", "import acme.shared.x\n"),
    ]);
    let index = resolve_path(temp.path());
    let resolved = index
        .imports
        .iter()
        .find(|resolved| resolved.full_module == "acme.shared.x")
        .expect("import");
    assert_eq!(resolved.distribution.as_deref(), Some("acme-b"));
}

#[test]
fn declared_distributions_with_differing_import_names_resolve() {
    // Distributions sharing a namespace with other mapped ones (#748).
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"databricks-sdk\", \"databricks-agents\", \"google-api-core\", \"google-cloud-firestore\", \"paddlepaddle-gpu\", \"faiss-gpu\", \"griffe\", \"llama-index-core\"]\n",
        ),
        ("app/__init__.py", ""),
        (
            "app/main.py",
            "import databricks.sdk.service.catalog\nfrom databricks.agents.evals import judges\nimport google.api_core.exceptions\nfrom google.cloud import firestore\nfrom google.cloud.firestore_v1.base_query import FieldFilter\nimport paddle\nimport faiss\nimport griffe\nfrom llama_index.core.schema import TextNode\n",
        ),
    ]);
    let index = resolve_path(temp.path());
    let distribution = |module: &str| {
        index
            .imports
            .iter()
            .find(|resolved| resolved.full_module == module)
            .map(|resolved| (resolved.origin, resolved.distribution.clone()))
            .expect("import")
    };
    for (module, expected) in [
        ("databricks.sdk.service.catalog", "databricks-sdk"),
        ("databricks.agents.evals", "databricks-agents"),
        ("google.api_core.exceptions", "google-api-core"),
        ("google.cloud", "google-cloud-firestore"),
        (
            "google.cloud.firestore_v1.base_query",
            "google-cloud-firestore",
        ),
        // paddlepaddle provides `paddle` too; the declared one wins.
        ("paddle", "paddlepaddle-gpu"),
        // faiss-cpu and griffelib provide `faiss` and `griffe` too (#780).
        ("faiss", "faiss-gpu"),
        ("griffe", "griffe"),
        ("llama_index.core.schema", "llama-index-core"),
    ] {
        assert_eq!(
            distribution(module),
            (ModuleOrigin::ThirdParty, Some(expected.into())),
            "{module}"
        );
    }
}

#[test]
fn declared_variant_distributions_own_their_shared_import() {
    // Without the environment, a variant the map lacked resolved to the
    // map's one distribution: CHK002 on the declaration, CHK003 on the
    // import, and `--fix --add-missing` swapped them (#1139).
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"opencv-python-headless\", \"msgpack-python\", \"psycopg2\", \"pillow-simd\"]\n",
        ),
        ("app/__init__.py", ""),
        (
            "app/main.py",
            "import cv2\nimport msgpack\nimport psycopg2\nfrom PIL import Image\n",
        ),
    ]);
    let index = resolve_path(temp.path());
    for (module, expected) in [
        ("cv2", "opencv-python-headless"),
        ("msgpack", "msgpack-python"),
        ("psycopg2", "psycopg2"),
        ("PIL", "pillow-simd"),
    ] {
        let resolved = index
            .imports
            .iter()
            .find(|resolved| resolved.full_module == module)
            .expect("import");
        assert_eq!(
            (resolved.origin, resolved.distribution.as_deref()),
            (ModuleOrigin::ThirdParty, Some(expected)),
            "{module}"
        );
    }
    // The declared variant settles each root, so none is ambiguous.
    assert!(
        !index.warnings.iter().any(|warning| matches!(
            warning,
            chokkin::internals::ResolveWarning::AmbiguousImport { .. }
        )),
        "{:?}",
        index.warnings
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
        .filter(|warning| {
            matches!(
                warning,
                chokkin::internals::ResolveWarning::AmbiguousImport { .. }
            )
        })
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
            .find(|resolved| &*resolved.import_root == name)
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
            .find(|resolved| &*resolved.import_root == name)
            .map_or_else(
                || panic!("{name} import"),
                |resolved| (resolved.origin, resolved.distribution.clone()),
            )
    };

    assert_eq!(
        root("foo_bar"),
        (ModuleOrigin::ThirdParty, Some("foo-bar".into()))
    );
    assert_eq!(
        root("Locked_Only"),
        (ModuleOrigin::ThirdParty, Some("locked-only".into()))
    );
    assert_eq!(root("e2e_config"), (ModuleOrigin::Unknown, None));
    assert!(index.warnings.iter().any(|warning| matches!(
        warning,
        chokkin::internals::ResolveWarning::UnresolvedImport { import, .. } if import == "e2e_config"
    )));
}

#[test]
fn affixed_declared_name_resolves_as_maybe() {
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"affix-demo\"\nversion = \"0.1.0\"\ndependencies = [\"widget-py\", \"pygadget\"]\n",
        ),
        (
            "uv.lock",
            "version = 1\n\n[[package]]\nname = \"widget-py\"\nversion = \"1.0\"\ndependencies = [{ name = \"python-doohickey\" }]\n\n[[package]]\nname = \"python-doohickey\"\nversion = \"1.0\"\n",
        ),
        (
            "app.py",
            "import widget\nimport gadget\nimport doohickey\nimport gizmo\n",
        ),
    ]);
    let index = resolve_path(temp.path());
    let root = |name: &str| {
        index
            .imports
            .iter()
            .find(|resolved| &*resolved.import_root == name)
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
            Some("widget-py".into()),
            ResolveConfidence::Maybe
        )
    );
    assert_eq!(root("gadget").1.as_deref(), Some("pygadget"));
    assert_eq!(root("doohickey").1.as_deref(), Some("python-doohickey"));
    assert_eq!(root("gizmo").0, ModuleOrigin::Unknown);
}

#[test]
fn build_requirement_resolves_without_a_distribution() {
    // sqlalchemy's `cython` build requirement (#720).
    let temp = temp_project(&[
        (
            "pyproject.toml",
            "[build-system]\nrequires = [\"cython>=3.3\", \"pywidget\"]\n\n\
             [project]\nname = \"build-demo\"\nversion = \"0.1.0\"\n",
        ),
        ("app.py", "import cython\nimport widget\nimport gizmo\n"),
    ]);
    let index = resolve_path(temp.path());
    let root = |name: &str| {
        index
            .imports
            .iter()
            .find(|resolved| &*resolved.import_root == name)
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
        root("cython"),
        (ModuleOrigin::ThirdParty, None, ResolveConfidence::Likely)
    );
    assert_eq!(
        root("widget"),
        (ModuleOrigin::ThirdParty, None, ResolveConfidence::Maybe)
    );
    assert_eq!(root("gizmo").0, ModuleOrigin::Unknown);
}

#[test]
fn pyodide_runtime_modules_resolve_unless_a_package_provides_them() {
    // urllib3's emscripten contrib imports what Pyodide itself provides (#722).
    let bare = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"runtime-demo\"\nversion = \"0.1.0\"\n",
        ),
        (
            "fetch.py",
            "import js\nimport pyodide_js\nimport _pyodide\nfrom pyodide.ffi import to_js\nimport jsx\n",
        ),
    ]);
    let index = resolve_path(bare.path());
    let root = |name: &str| {
        index
            .imports
            .iter()
            .find(|resolved| &*resolved.import_root == name)
            .map(|resolved| (resolved.origin, resolved.distribution.clone()))
            .expect("import")
    };
    for name in ["js", "pyodide_js", "_pyodide", "pyodide"] {
        assert_eq!(root(name), (ModuleOrigin::ThirdParty, None), "{name}");
    }
    assert_eq!(root("jsx"), (ModuleOrigin::Unknown, None));

    // A declared or mapped package of the same name still names its distribution.
    let packaged = temp_project(&[
        (
            "pyproject.toml",
            "[project]\nname = \"runtime-demo\"\nversion = \"0.1.0\"\n\
             dependencies = [\"pyodide-py\"]\n\n\
             [tool.chokkin.package_module_map]\n\"js-shim\" = [\"js\"]\n",
        ),
        ("fetch.py", "import js\nimport pyodide\n"),
    ]);
    let index = resolve_path(packaged.path());
    let distribution = |name: &str| {
        index
            .imports
            .iter()
            .find(|resolved| &*resolved.import_root == name)
            .and_then(|resolved| resolved.distribution.clone())
    };
    assert_eq!(distribution("js").as_deref(), Some("js-shim"));
    assert_eq!(distribution("pyodide").as_deref(), Some("pyodide-py"));
}

#[test]
fn pytest_plugins_take_file_context_and_stay_quiet_when_unresolved() {
    let index = resolve_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/reachability/pytest_plugins_var"),
    );
    let context = |file: &str| {
        index
            .imports
            .iter()
            .find(|resolved| resolved.full_module == "pytest_asyncio" && &*resolved.file == file)
            .map(|resolved| resolved.context)
    };
    assert_eq!(
        context("tests/conftest.py"),
        Some(chokkin::internals::ImportContext::Test)
    );
    assert_eq!(
        context("src/acme/__init__.py"),
        Some(chokkin::internals::ImportContext::Runtime)
    );
    assert!(!index.warnings.iter().any(|warning| matches!(
        warning,
        chokkin::internals::ResolveWarning::UnresolvedImport { import, .. } if import == "pytest_asyncio"
    )));

    // A `pytest_plugins` string is a config reference, not an `import`, so
    // messages name the setting and statement-keyed indexes skip it (#830).
    let plugin = index
        .imports
        .iter()
        .find(|resolved| {
            resolved.full_module == "pytest_asyncio" && &*resolved.file == "tests/conftest.py"
        })
        .expect("pytest_asyncio reference");
    assert_eq!(plugin.reference.as_deref(), Some("pytest_plugins"));
    assert_eq!(plugin.line, Some(1));
    assert_eq!(plugin.statement_line(), None);
}

#[test]
fn dead_stdlib_fallback_never_runs_and_softens_the_unresolved_warning() {
    let index = resolve_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/symbols/dead_stdlib_fallback"),
    );
    let site = index
        .imports
        .iter()
        .find(|resolved| &*resolved.import_root == "dummy_threading")
        .expect("dummy_threading import");
    // Not optional: it is not "used when installed" but never run (#1027).
    assert!(!site.optional);
    assert!(site.never_runs);
    assert!(index.warnings.iter().any(|warning| matches!(
        warning,
        chokkin::internals::ResolveWarning::UnresolvedImport { import, optional: true, .. }
            if import == "dummy_threading"
    )));
}

#[test]
fn warnings_follow_module_order_across_many_files() {
    // Enough files for resolution to spread over workers: an ambiguous root
    // first imported mid-way still warns once, at its first importer.
    let pyproject = "[project]\nname = \"app\"\nversion = \"0.1.0\"\n\n[tool.chokkin.package_module_map]\n\"dual-a\" = [\"dual\"]\n\"dual-b\" = [\"dual\"]\n\"ns-a\" = [\"ns.shared\"]\n\"ns-b\" = [\"ns.shared\"]\n";
    let mut files = vec![
        ("pyproject.toml".to_owned(), pyproject.to_owned()),
        ("app/__init__.py".to_owned(), String::new()),
    ];
    for index in 0..80 {
        let mut source = String::new();
        if index == 40 || index > 50 {
            source.push_str("import dual\n");
        }
        if index >= 60 {
            source.push_str("import ns.shared.x\n");
        }
        files.push((
            format!("app/m{index:02}.py"),
            source + &format!("import missing_{index:02}\n"),
        ));
    }
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    let temp = temp_project(&borrowed);
    let index = resolve_path(temp.path());

    let mut expected = Vec::new();
    for module in 0..80 {
        if module == 40 {
            expected.push("ambiguous dual".to_owned());
        }
        if module == 60 {
            expected.push("ambiguous ns".to_owned());
        }
        expected.push(format!("unresolved missing_{module:02}"));
    }
    let actual: Vec<String> = index
        .warnings
        .iter()
        .map(|warning| match warning {
            chokkin::internals::ResolveWarning::AmbiguousImport { import, .. } => {
                format!("ambiguous {import}")
            },
            chokkin::internals::ResolveWarning::UnresolvedImport { import, .. } => {
                format!("unresolved {import}")
            },
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn pytest_plugin_sites_take_part_in_warning_order_across_many_files() {
    // A `pytest_plugins` entry that is the first lookup of an ambiguous root
    // reports it there; its own unresolved names stay quiet.
    let pyproject = "[project]\nname = \"app\"\nversion = \"0.1.0\"\n\n[tool.chokkin.package_module_map]\n\"dual-a\" = [\"dual\"]\n\"dual-b\" = [\"dual\"]\n";
    let mut files = vec![
        ("pyproject.toml".to_owned(), pyproject.to_owned()),
        ("app/__init__.py".to_owned(), String::new()),
    ];
    for index in 0..80 {
        let mut source = String::new();
        if index == 30 {
            source.push_str("pytest_plugins = [\"dual\", \"missing_plugin\"]\n");
        }
        if index == 40 || index > 50 {
            source.push_str("import dual\n");
        }
        files.push((
            format!("app/m{index:02}.py"),
            source + &format!("import missing_{index:02}\n"),
        ));
    }
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    let temp = temp_project(&borrowed);
    let index = resolve_path(temp.path());

    let mut expected = Vec::new();
    for module in 0..80 {
        expected.push(format!("unresolved missing_{module:02}"));
        if module == 30 {
            expected.push("ambiguous dual".to_owned());
        }
    }
    let actual: Vec<String> = index
        .warnings
        .iter()
        .map(|warning| match warning {
            chokkin::internals::ResolveWarning::AmbiguousImport { import, .. } => {
                format!("ambiguous {import}")
            },
            chokkin::internals::ResolveWarning::UnresolvedImport { import, .. } => {
                format!("unresolved {import}")
            },
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(actual, expected);
}

/// §7 steps 0 and 1 against a table (#1109): a declared backport is the
/// import's distribution on every `requires-python`, the other backports of
/// the module included; otherwise a root any minor of the window ships is
/// stdlib, and one only other minors ship resolves to its backports like a
/// third-party import, or to nothing without one.
#[test]
fn removed_stdlib_modules_follow_the_backport_table() {
    // (root, minors that ship it, backports in the bundled map)
    let roots: [(&str, (u32, u32), &[&str]); 4] = [
        ("cgi", (8, 12), &["legacy-cgi", "standard-cgi"]),
        ("asynchat", (8, 11), &["standard-asynchat"]),
        ("tomllib", (11, 14), &[]),
        ("os", (8, 14), &[]),
    ];
    // (requires-python, the minors it puts in the window); the probe sets
    // `target_version` from its lower bound.
    let ranges = [
        (">=3.8,<3.11", (8, 10)),
        (">=3.11,<3.12", (11, 11)),
        (">=3.12,<3.13", (12, 12)),
        (">=3.13", (13, 14)),
        (">=3.9", (9, 14)),
    ];
    let declarations = [
        None,
        Some("legacy-cgi"),
        Some("standard-cgi"),
        Some("standard-asynchat"),
        Some("zz-other"),
    ];
    for (root, ships, backports) in roots {
        for (requires, window) in ranges {
            for declared in declarations {
                let deps = declared.map_or_else(String::new, |name| format!("\"{name}\""));
                let temp = temp_project(&[
                    (
                        "pyproject.toml",
                        &format!(
                            "[project]\nname = \"acme\"\nversion = \"0.1.0\"\nrequires-python = \"{requires}\"\ndependencies = [{deps}]\n\n[tool.chokkin]\ntarget_version = \"py3{:02}\"\n",
                            window.0
                        ),
                    ),
                    ("main.py", &format!("import {root}\n")),
                ]);
                let index = resolve_path(temp.path());
                let import = index
                    .imports
                    .iter()
                    .find(|import| &*import.import_root == root)
                    .expect("the import resolves");
                let case = format!("{root} with {requires} declaring {declared:?}");
                let stdlib = ships.0 <= window.1 && window.0 <= ships.1;
                match declared.filter(|name| backports.contains(name)) {
                    Some(backport) => {
                        assert_eq!(import.origin, ModuleOrigin::ThirdParty, "{case}");
                        assert_eq!(import.distribution.as_deref(), Some(backport), "{case}");
                        assert_eq!(import.confidence, ResolveConfidence::Certain, "{case}");
                    },
                    None if stdlib => assert_eq!(import.origin, ModuleOrigin::Stdlib, "{case}"),
                    None if backports.is_empty() => {
                        assert_eq!(import.origin, ModuleOrigin::Unknown, "{case}");
                    },
                    None => {
                        assert_eq!(import.origin, ModuleOrigin::ThirdParty, "{case}");
                        // Several backports warn and name the first.
                        assert_eq!(import.distribution.as_deref(), Some(backports[0]), "{case}");
                    },
                }
            }
        }
    }
}
