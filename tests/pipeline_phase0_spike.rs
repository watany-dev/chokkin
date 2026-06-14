//! End-to-end Phase 0 pipeline: discover → graph skeleton → parse → import edges.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use chokkin::{
    GraphEdge, ProjectRoot, RootMarker, add_parsed_imports, build_graph_skeleton,
    discover_project_root, discover_sources, extract_manifest, load_config, parse_file,
    resolve_target_version,
};

fn sources_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sources")
        .join(name)
}

#[test]
fn pipeline_phase0_spike() -> Result<(), Box<dyn std::error::Error>> {
    let path = sources_fixture("src_layout");
    let root = discover_project_root(&path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()),
        marker: RootMarker::PyProjectToml,
        start: path,
    });
    let loaded = load_config(&root)?;
    let manifest = extract_manifest(&root, &loaded)?;
    let sources = discover_sources(&root, &loaded, &manifest)?;
    let target = resolve_target_version(&loaded.effective, &manifest);
    let mut graph = build_graph_skeleton(&manifest, &sources)?;

    for file in sources.python_files() {
        let parsed = parse_file(&root, &file.path, &sources.layout, file.context, &target)?;
        let file_id = graph
            .file_id(&file.path)
            .ok_or("discovered file missing from graph")?;
        add_parsed_imports(&mut graph, file_id, &parsed)?;
    }

    assert!(graph.edges().iter().any(|edge| {
        matches!(edge, GraphEdge::FileImportsModule { .. })
            || matches!(edge, GraphEdge::ManifestDeclaresDistribution { .. })
    }));
    Ok(())
}
