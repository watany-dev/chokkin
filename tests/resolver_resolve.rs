//! Integration tests for import resolution (pipeline step 7).

#![allow(clippy::expect_used, clippy::unwrap_used)]

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
    let plugins = extract_plugin_hints(&root, &loaded, &sources, &manifest).expect("plugins");
    let plugin_refs: Vec<_> = plugins.module_refs().cloned().collect();
    resolve_imports(
        &root,
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &plugin_refs,
        &loaded.workspace_members,
    )
    .expect("resolve")
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
