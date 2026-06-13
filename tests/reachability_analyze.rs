//! Integration tests for reachability analysis (pipeline step 9).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use yokei::{
    Confidence, GraphEdge, ProjectMode, ProjectRoot, RootMarker, add_parsed_imports,
    analyze_reachability, apply_entry_plan, apply_resolution_to_graph, build_entry_roots,
    build_graph_skeleton, discover_project_root, discover_sources, extract_manifest,
    extract_plugin_hints, load_config, parse_project_sources, resolve_imports,
    resolve_target_version, trace_to_file,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/reachability")
        .join(name)
}

fn plugins_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plugins")
        .join(name)
}

struct ReachabilityInputs {
    sources: yokei::DiscoveredSources,
    plugins: yokei::PluginHints,
    parse: yokei::ParseSummary,
    entry: yokei::EntryPlan,
    graph: yokei::ProjectGraph,
}

fn load_reachability(path: &Path, production: bool) -> ReachabilityInputs {
    let root = discover_project_root(path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        marker: RootMarker::PyProjectToml,
        start: path.to_path_buf(),
    });
    let loaded = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &loaded).expect("extract manifest");
    let sources = discover_sources(&root, &loaded, &manifest).expect("discover sources");
    let plugins = extract_plugin_hints(&root, &loaded, &sources, &manifest).expect("plugin hints");
    let target = resolve_target_version(&loaded.effective, &manifest);
    let parse = parse_project_sources(&root, &sources, &target).expect("parse");
    let entry = build_entry_roots(&loaded.effective, &manifest, &sources, &plugins, production)
        .expect("entry plan");

    let mut graph = build_graph_skeleton(&manifest, &sources).expect("graph skeleton");
    for module in &parse.modules {
        let file_id = graph.file_id(&module.path).expect("file id");
        add_parsed_imports(&mut graph, file_id, module).expect("parsed imports");
    }
    let plugin_refs: Vec<_> = plugins.module_refs().cloned().collect();
    for reference in &plugin_refs {
        let _ = graph.intern_module(reference.module.clone(), yokei::ModuleOrigin::Unknown);
    }
    let resolution = resolve_imports(
        &root,
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &plugin_refs,
    )
    .expect("resolve imports");
    apply_resolution_to_graph(&mut graph, &resolution).expect("apply resolution");
    apply_entry_plan(&mut graph, &entry).expect("apply entry plan");

    ReachabilityInputs {
        sources,
        plugins,
        parse,
        entry,
        graph,
    }
}

#[test]
fn chain_import_reaches_transitive_modules() {
    let mut inputs = load_reachability(&fixture("chain_import"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    assert_eq!(inputs.entry.mode.mode, ProjectMode::App);
    assert!(
        report
            .reachable
            .contains(&inputs.graph.file_id("src/acme/main.py").expect("main"))
    );
    assert!(
        report
            .reachable
            .contains(&inputs.graph.file_id("src/acme/a.py").expect("a"))
    );
    assert!(
        report
            .reachable
            .contains(&inputs.graph.file_id("src/acme/b.py").expect("b"))
    );
    assert!(
        report
            .unreachable
            .iter()
            .any(|file| file.path == "src/acme/legacy.py")
    );
    assert!(
        inputs
            .graph
            .edges()
            .iter()
            .any(|edge| matches!(edge, GraphEdge::FileReachesFile { .. }))
    );
}

#[test]
fn orphan_file_is_unreachable_in_app_mode() {
    let mut inputs = load_reachability(&fixture("chain_import"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    let legacy = report
        .unreachable
        .iter()
        .find(|file| file.path == "src/acme/legacy.py")
        .expect("legacy");
    assert_eq!(legacy.max_confidence, Confidence::Certain);
}

#[test]
fn library_mode_caps_orphan_confidence() {
    let mut inputs = load_reachability(&fixture("library_orphan"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    assert_eq!(inputs.entry.mode.mode, ProjectMode::Library);
    let orphan = report
        .unreachable
        .iter()
        .find(|file| file.path == "src/acme/orphan.py")
        .expect("orphan");
    assert_eq!(orphan.max_confidence, Confidence::Maybe);
}

#[test]
fn plugin_module_reference_reaches_app_package() {
    let mut inputs = load_reachability(&fixture("plugin_module_ref"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    assert!(
        report.reachable.contains(
            &inputs
                .graph
                .file_id("myapp/__init__.py")
                .expect("myapp init")
        )
    );
    assert!(
        inputs
            .graph
            .edges()
            .iter()
            .any(|edge| matches!(edge, GraphEdge::ConfigReferenceUsesModule { .. }))
    );
}

#[test]
fn dynamic_literal_import_reaches_target_module() {
    let mut inputs = load_reachability(&fixture("dynamic_import"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    assert!(
        report.reachable.contains(
            &inputs
                .graph
                .file_id("src/acme/plugins.py")
                .expect("plugins")
        )
    );
}

#[test]
fn django_migrations_are_framework_used() {
    let mut inputs = load_reachability(&plugins_fixture("django_migrations"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    let migration = inputs
        .graph
        .file_id("mysite/migrations/0001_initial.py")
        .expect("migration");
    assert!(report.framework_used.contains(&migration));
    assert!(report.reachable.contains(&migration));
    assert!(
        !report
            .unreachable
            .iter()
            .any(|file| file.path == "mysite/migrations/0001_initial.py")
    );
}

#[test]
fn trace_to_file_returns_import_chain() {
    let mut inputs = load_reachability(&fixture("chain_import"), false);
    let report = analyze_reachability(
        &mut inputs.graph,
        &inputs.sources,
        &inputs.entry,
        &inputs.plugins,
        &inputs.parse,
        &inputs.entry.mode,
        false,
    )
    .expect("reachability");

    let target = inputs.graph.file_id("src/acme/b.py").expect("target file");
    let trace = trace_to_file(&report, target).expect("trace");
    assert_eq!(trace.target, target);
    assert!(!trace.steps.is_empty());
}
