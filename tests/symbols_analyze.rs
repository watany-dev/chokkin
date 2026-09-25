//! Integration tests for symbol usage analysis (pipeline step 11).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use chokkin::{
    Confidence, ProjectRoot, RootMarker, RuleId, Severity, add_parsed_imports,
    analyze_reachability, analyze_symbols, apply_entry_plan, apply_resolution_to_graph,
    build_entry_roots, build_graph_skeleton, discover_project_root, discover_sources,
    extract_manifest, extract_plugin_hints, load_config, parse_project_sources, resolve_imports,
    resolve_target_version,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/symbols")
        .join(name)
}

struct SymbolInputs {
    manifest: chokkin::LoadedManifest,
    sources: chokkin::DiscoveredSources,
    plugins: chokkin::PluginHints,
    parse: chokkin::ParseSummary,
    graph: chokkin::ProjectGraph,
    resolution: chokkin::ResolutionIndex,
    reachability: chokkin::ReachabilityReport,
    entry: chokkin::EntryPlan,
}

fn load_symbols(path: &Path, production: bool) -> SymbolInputs {
    let root = discover_project_root(path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        marker: RootMarker::PyProjectToml,
        start: path.to_path_buf(),
    });
    let loaded = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &loaded).expect("extract manifest");
    let sources = discover_sources(&root, &loaded, &manifest).expect("discover sources");
    let target = resolve_target_version(&loaded.effective, &manifest);
    let parse = parse_project_sources(&root, &sources, &target).expect("parse");
    let plugins =
        extract_plugin_hints(&root, &loaded, &sources, &manifest, &parse).expect("plugin hints");
    let entry = build_entry_roots(&loaded.effective, &manifest, &sources, &plugins, production);

    let mut graph = build_graph_skeleton(&manifest, &sources).expect("graph skeleton");
    for module in &parse.modules {
        let file_id = graph.file_id(&module.path).expect("file id");
        add_parsed_imports(&mut graph, file_id, module).expect("parsed imports");
    }
    let plugin_refs: Vec<_> = plugins.module_refs().cloned().collect();
    for reference in &plugin_refs {
        let _ = graph.intern_module(reference.module.clone(), chokkin::ModuleOrigin::Unknown);
    }
    let resolution = resolve_imports(
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &plugin_refs,
        &loaded.workspace_members,
    );
    apply_resolution_to_graph(&mut graph, &resolution).expect("apply resolution");
    apply_entry_plan(&mut graph, &entry);
    let reachability = analyze_reachability(
        &mut graph,
        &sources,
        &entry,
        &plugins,
        &parse,
        &entry.mode,
        production,
    )
    .expect("reachability");

    SymbolInputs {
        manifest,
        sources,
        plugins,
        parse,
        graph,
        resolution,
        reachability,
        entry,
    }
}

fn analyze_fixture(name: &str) -> chokkin::SymbolReport {
    let inputs = load_symbols(&fixture(name), false);
    analyze_symbols(
        &inputs.parse,
        &inputs.resolution,
        &inputs.reachability,
        &inputs.entry,
        &inputs.plugins,
        &inputs.entry.mode,
        &inputs.graph,
        &inputs.sources,
        &inputs.manifest,
    )
}

fn has_symbol_rule(report: &chokkin::SymbolReport, rule: RuleId, module: &str, name: &str) -> bool {
    report.candidates.iter().any(|candidate| {
        candidate.rule == rule
            && matches!(
                &candidate.subject,
                chokkin::IssueSubject::Symbol { module: m, name: n }
                    if m == module && n == name
            )
    })
}

#[test]
fn unused_public_function_emits_chk006() {
    let report = analyze_fixture("unused_export");
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.utils",
        "dead_api"
    ));
    assert!(!has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.utils",
        "helper"
    ));
    let dead = report
        .candidates
        .iter()
        .find(|candidate| {
            candidate.rule == RuleId::Chk006
                && matches!(
                    &candidate.subject,
                    chokkin::IssueSubject::Symbol { name, .. } if name == "dead_api"
                )
        })
        .expect("dead_api candidate");
    assert_eq!(dead.severity, Severity::Warning);
    assert_eq!(dead.confidence, Confidence::Likely);
}

#[test]
fn pytest_fixture_is_not_reported() {
    let report = analyze_fixture("pytest_fixture");
    assert!(!has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.conftest",
        "sample_data"
    ));
    assert!(
        report
            .external_symbols
            .iter()
            .any(|symbol| { symbol.module == "acme.conftest" && symbol.name == "sample_data" })
    );
}

#[test]
fn fastapi_route_and_websocket_handlers_are_external() {
    let report = analyze_fixture("fastapi_websocket");
    for name in ["list_items", "stream"] {
        assert!(
            !has_symbol_rule(&report, RuleId::Chk006, "acme.routes", name),
            "{name}"
        );
    }
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.routes",
        "dead_api"
    ));
}

#[test]
fn unused_reexport_emits_chk007() {
    let report = analyze_fixture("unused_reexport");
    assert!(has_symbol_rule(&report, RuleId::Chk007, "acme", "foo"));
    assert!(has_symbol_rule(&report, RuleId::Chk007, "acme", "helpers"));
}

#[test]
fn reexport_source_module_is_resolved_once() {
    let report = analyze_fixture("unused_reexport");
    let source_module = |name: &str| {
        let candidate = report
            .candidates
            .iter()
            .find(|candidate| {
                candidate.rule == RuleId::Chk007
                    && matches!(
                        &candidate.subject,
                        chokkin::IssueSubject::Symbol { name: n, .. } if n == name
                    )
            })
            .expect("CHK007 candidate");
        match candidate.origins.first() {
            Some(chokkin::Origin::Import { module, .. }) => module.clone(),
            other => panic!("unexpected origin: {other:?}"),
        }
    };
    // `from .sub import foo`
    assert_eq!(source_module("foo"), "acme.sub");
    // `from . import helpers`
    assert_eq!(source_module("helpers"), "acme.helpers");
}

#[test]
fn unresolved_import_emits_chk010() {
    let report = analyze_fixture("unresolved_import");
    assert!(report.candidates.iter().any(|candidate| {
        candidate.rule == RuleId::Chk010
            && matches!(
                &candidate.subject,
                chokkin::IssueSubject::Import { module, .. } if module == "notarealpkg"
            )
    }));
}

#[test]
fn library_mode_downgrades_chk006_to_info() {
    let report = analyze_fixture("library_mode");
    let unused = report
        .candidates
        .iter()
        .find(|candidate| {
            candidate.rule == RuleId::Chk006
                && matches!(
                    &candidate.subject,
                    chokkin::IssueSubject::Symbol { name, .. } if name == "unused_public"
                )
        })
        .expect("unused_public candidate");
    assert_eq!(unused.severity, Severity::Info);
}

#[test]
fn library_mode_unshipped_package_keeps_chk006_warning() {
    let report = analyze_fixture("library_wheel_targets");
    let severity_of = |symbol: &str| {
        report
            .candidates
            .iter()
            .find(|candidate| {
                candidate.rule == RuleId::Chk006
                    && matches!(
                        &candidate.subject,
                        chokkin::IssueSubject::Symbol { name, .. } if name == symbol
                    )
            })
            .map(|candidate| candidate.severity)
    };
    assert_eq!(severity_of("unused_public"), Some(Severity::Info));
    assert_eq!(severity_of("unused_internal"), Some(Severity::Warning));
}

#[test]
fn import_module_attribute_access_counts_as_external_reference() {
    let report = analyze_fixture("import_attr_access");
    assert!(!has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.utils",
        "helper"
    ));
    assert!(has_symbol_rule(
        &report,
        RuleId::Chk006,
        "acme.utils",
        "dead_api"
    ));
}
