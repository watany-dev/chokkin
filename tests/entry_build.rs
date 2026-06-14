//! Integration tests for entry root construction (pipeline step 8).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use chokkin::{
    EntryOrigin, EntryWarning, GraphEdge, ProjectMode, ProjectRoot, RootMarker, apply_entry_plan,
    build_entry_roots, build_graph_skeleton, discover_project_root, discover_sources,
    extract_manifest, extract_plugin_hints, load_config,
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
        start: path.to_path_buf(),
    }
}

struct PipelineInputs {
    config: chokkin::ChokkinConfig,
    manifest: chokkin::LoadedManifest,
    sources: chokkin::DiscoveredSources,
    plugins: chokkin::PluginHints,
}

fn load_pipeline(path: &Path) -> PipelineInputs {
    let root = discover_project_root(path).unwrap_or_else(|_| project_root_at(path));
    let loaded = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &loaded).expect("extract manifest");
    let sources = discover_sources(&root, &loaded, &manifest).expect("discover sources");
    let plugins = extract_plugin_hints(&root, &loaded, &sources, &manifest).expect("plugin hints");
    let config = loaded.effective;
    PipelineInputs {
        config,
        manifest,
        sources,
        plugins,
    }
}

fn entry_paths(plan: &chokkin::EntryPlan) -> Vec<&str> {
    plan.roots
        .iter()
        .map(|root| root.spec.path.as_str())
        .collect()
}

#[test]
fn django_manage_is_entry_with_plugin_origins() {
    let inputs = load_pipeline(&plugins_fixture("django_manage"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &inputs.plugins,
        false,
    )
    .expect("entry plan");

    assert_eq!(plan.mode.mode, ProjectMode::App);
    let manage = plan
        .roots
        .iter()
        .find(|root| root.spec.path == "manage.py")
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
        &inputs.plugins,
        false,
    )
    .expect("entry plan");

    let paths = entry_paths(&plan);
    assert!(paths.iter().any(|path| path.contains("asgi.py")));
    assert_eq!(plan.mode.mode, ProjectMode::App);
}

#[test]
fn library_only_resolves_library_mode() {
    let inputs = load_pipeline(&fixture("library_only"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &inputs.plugins,
        false,
    )
    .expect("entry plan");

    assert_eq!(plan.mode.mode, ProjectMode::Library);
}

#[test]
fn explicit_config_entry_merges_with_auto() {
    let inputs = load_pipeline(&fixture("explicit_entry"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &inputs.plugins,
        false,
    )
    .expect("entry plan");

    let manage = plan
        .roots
        .iter()
        .find(|root| root.spec.path == "manage.py")
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
        &inputs.plugins,
        false,
    )
    .expect("entry plan");

    assert!(
        plan.warnings
            .iter()
            .any(|warning| matches!(warning, EntryWarning::MissingEntryPath { .. }))
    );
    assert!(!plan.roots.iter().any(|root| root.spec.path == "missing.py"));
}

#[test]
fn production_excludes_test_context_entries() {
    let inputs = load_pipeline(&plugins_fixture("pytest_pyproject"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &inputs.plugins,
        true,
    )
    .expect("entry plan");

    assert!(
        plan.roots
            .iter()
            .all(|root| root.context.is_included_in_production())
    );
}

#[test]
fn apply_entry_plan_adds_graph_edges() {
    let inputs = load_pipeline(&plugins_fixture("django_manage"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &inputs.plugins,
        false,
    )
    .expect("entry plan");
    let mut graph =
        build_graph_skeleton(&inputs.manifest, &inputs.sources).expect("graph skeleton");
    apply_entry_plan(&mut graph, &plan).expect("apply entry plan");

    assert!(graph.entry_count() > 0);
    assert!(
        graph
            .edges()
            .iter()
            .any(|edge| matches!(edge, GraphEdge::EntryReachesFile { .. }))
    );
}

#[test]
fn manifest_script_resolves_to_module_file() {
    let inputs = load_pipeline(&plugins_fixture("fastapi_scripts"));
    let plan = build_entry_roots(
        &inputs.config,
        &inputs.manifest,
        &inputs.sources,
        &inputs.plugins,
        false,
    )
    .expect("entry plan");

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
    assert_eq!(entry.spec.path, "src/pkg/main.py");
    assert_eq!(entry.spec.symbol.as_deref(), Some("app"));
}
