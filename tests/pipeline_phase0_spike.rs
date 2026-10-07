//! End-to-end Phase 0 pipeline: discover → graph skeleton → parse → import edges.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use chokkin::internals::{
    GraphEdge, ProjectRoot, RootMarker, add_parsed_imports, build_graph_skeleton,
    discover_project_root, discover_sources, extract_manifest, load_config, parse_file,
    resolve_target_version,
};

fn deps_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deps")
        .join(name)
}

#[test]
fn pipeline_phase0_spike() -> Result<(), Box<dyn std::error::Error>> {
    // `sources/src_layout` has only empty files, so it cannot exercise import edges.
    let path = deps_fixture("unused_boto3");
    let root = discover_project_root(&path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()),
        marker: RootMarker::PyProjectToml,
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

    let mut imports = Vec::new();
    for edge in graph.edges() {
        let GraphEdge::FileImportsModule { file, module, line } = edge else {
            panic!("unexpected Phase 0 edge: {edge:?}");
        };
        imports.push((
            graph.file(*file).map(|node| node.path.as_str()),
            graph.module(*module).map(|node| node.name.as_str()),
            *line,
        ));
    }
    assert_eq!(imports, [(Some("src/acme/main.py"), Some("requests"), 1)]);
    assert_eq!(graph.distribution_count(), 2);
    graph.distribution_id("boto3").ok_or("boto3")?;
    graph.distribution_id("requests").ok_or("requests")?;
    Ok(())
}
