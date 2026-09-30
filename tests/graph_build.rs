//! Integration tests for graph skeleton construction.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use chokkin::{
    FileContext, FileKind, ModuleOrigin, ProjectRoot, RootMarker, build_graph_skeleton,
    discover_project_root, discover_sources, extract_manifest, load_config,
};

fn sources_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sources")
        .join(name)
}

fn pipeline_inputs(name: &str) -> (chokkin::LoadedManifest, chokkin::DiscoveredSources) {
    let path = sources_fixture(name);
    let root = discover_project_root(&path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()),
        marker: RootMarker::PyProjectToml,
    });
    let config = load_config(&root).expect("config");
    let manifest = extract_manifest(&root, &config).expect("manifest");
    let sources = discover_sources(&root, &config, &manifest).expect("sources");
    (manifest, sources)
}

#[test]
fn build_graph_registers_files_and_dependencies() {
    let (manifest, sources) = pipeline_inputs("src_layout");
    let graph = build_graph_skeleton(&manifest, &sources).expect("graph");

    // Src layout globs cover `src/`, `tests/` and `scripts/`, so `docs/conf.py` is not a node.
    let files: Vec<_> = graph
        .files()
        .map(|(_, node)| (node.path.as_str(), node.context, node.kind))
        .collect();
    assert_eq!(
        files,
        [
            ("scripts/run.py", FileContext::Dev, FileKind::Python),
            (
                "src/acme/__init__.py",
                FileContext::Runtime,
                FileKind::Python
            ),
            ("src/acme/module.py", FileContext::Runtime, FileKind::Python),
            ("tests/conftest.py", FileContext::Test, FileKind::Python),
            ("tests/test_module.py", FileContext::Test, FileKind::Python),
        ]
    );

    assert_eq!(graph.module_count(), 1);
    let acme = graph.module_id("acme").expect("first-party package module");
    assert_eq!(
        graph.module(acme).map(|node| node.origin),
        Some(ModuleOrigin::FirstParty)
    );

    assert_eq!(graph.distribution_count(), 1);
    assert!(graph.distribution_id("requests").is_some());
    assert!(graph.edges().is_empty());
}

#[test]
fn build_graph_skips_opaque_dependencies() {
    let (mut manifest, sources) = pipeline_inputs("src_layout");
    let mut opaque = manifest.dependencies[0].clone();
    opaque.name = "localpkg".to_owned();
    opaque.opaque = true;
    manifest.dependencies.push(opaque);

    let graph = build_graph_skeleton(&manifest, &sources).expect("graph");
    assert_eq!(graph.distribution_count(), 1);
    assert!(graph.distribution_id("localpkg").is_none());
}

#[test]
fn duplicate_files_are_rejected() {
    let (manifest, sources) = pipeline_inputs("src_layout");
    let mut sources = sources;
    if let Some(first) = sources.files.first() {
        sources.files.push(first.clone());
    }
    let error = build_graph_skeleton(&manifest, &sources).expect_err("duplicate");
    assert!(matches!(error, chokkin::GraphError::DuplicateFile { .. }));
}
