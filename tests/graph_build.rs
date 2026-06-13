//! Integration tests for graph skeleton construction.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use yokei::{
    GraphEdge, ProjectRoot, RootMarker, build_graph_skeleton, discover_project_root,
    discover_sources, extract_manifest, load_config,
};

fn sources_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sources")
        .join(name)
}

fn pipeline_inputs(name: &str) -> (yokei::LoadedManifest, yokei::DiscoveredSources) {
    let path = sources_fixture(name);
    let root = discover_project_root(&path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()),
        marker: RootMarker::PyProjectToml,
        start: path,
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
    assert!(graph.file_count() > 0);
    assert!(graph.distribution_count() > 0);
    assert!(
        graph
            .edges()
            .iter()
            .any(|edge| matches!(edge, GraphEdge::ManifestDeclaresDistribution { .. }))
    );
}

#[test]
fn duplicate_files_are_rejected() {
    let (manifest, sources) = pipeline_inputs("src_layout");
    let mut sources = sources;
    if let Some(first) = sources.files.first() {
        sources.files.push(first.clone());
    }
    let error = build_graph_skeleton(&manifest, &sources).expect_err("duplicate");
    assert!(matches!(error, yokei::GraphError::DuplicateFile { .. }));
}
