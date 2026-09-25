//! Integration tests for import resolution (pipeline step 7).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use chokkin::{
    ModuleOrigin, ProjectRoot, ResolveConfidence, RootMarker, discover_project_root,
    discover_sources, extract_manifest, extract_plugin_hints, load_config, parse_project_sources,
    resolve_imports, resolve_target_version,
};

fn resolver_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/resolver")
        .join(name)
}

fn resolve_fixture(name: &str) -> chokkin::ResolutionIndex {
    let path = resolver_fixture(name);
    let root = discover_project_root(&path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()),
        marker: RootMarker::PyProjectToml,
        start: path.clone(),
    });
    let loaded = load_config(&root).expect("config");
    let manifest = extract_manifest(&root, &loaded).expect("manifest");
    let sources = discover_sources(&root, &loaded, &manifest).expect("sources");
    let target = resolve_target_version(&loaded.effective, &manifest);
    let parse = parse_project_sources(&root, &sources, &target).expect("parse");
    let plugins =
        extract_plugin_hints(&root, &loaded, &sources, &manifest, &parse).expect("plugins");
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
        ("scripts/old.py", block(">=3.10")),
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
    let parse = parse_project_sources(&root, &sources, &target).expect("parse");
    let (scripts, warnings) = chokkin::discover_inline_scripts(
        &root.path,
        sources.python_files().map(|file| file.path.as_str()),
    );
    assert!(warnings.is_empty());
    let script_targets: BTreeMap<_, _> = scripts
        .iter()
        .filter_map(|script| Some((script.path.clone(), script.target_version.clone()?)))
        .collect();
    let index = chokkin::resolver::resolve_imports_with_script_targets(
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &[],
        &loaded.workspace_members,
        &script_targets,
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
    assert_ne!(tomllib_in("app.py").origin, ModuleOrigin::Stdlib);
}
