//! Integration tests for dependency reconciliation (pipeline step 10).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use yokei::{
    Confidence, GraphEdge, ProjectRoot, RootMarker, RuleId, Severity, add_parsed_imports,
    analyze_reachability, apply_entry_plan, apply_resolution_to_graph, build_entry_roots,
    build_graph_skeleton, discover_project_root, discover_sources, extract_manifest,
    extract_plugin_hints, load_config, parse_project_sources, reconcile_dependencies,
    resolve_imports, resolve_target_version,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deps")
        .join(name)
}

struct DepsInputs {
    manifest: yokei::LoadedManifest,
    config: yokei::YokeiConfig,
    sources: yokei::DiscoveredSources,
    plugins: yokei::PluginHints,
    parse: yokei::ParseSummary,
    graph: yokei::ProjectGraph,
    resolution: yokei::ResolutionIndex,
    reachability: yokei::ReachabilityReport,
}

fn load_deps(path: &Path, production: bool) -> DepsInputs {
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

    DepsInputs {
        manifest,
        config: loaded.effective,
        sources,
        plugins,
        parse,
        graph,
        resolution,
        reachability,
    }
}

fn reconcile_fixture(name: &str) -> yokei::DependencyReport {
    let inputs = load_deps(&fixture(name), false);
    reconcile_dependencies(
        &inputs.manifest,
        &inputs.resolution,
        &inputs.reachability,
        &inputs.plugins,
        &inputs.config,
        &inputs.sources,
        &inputs.parse,
        &inputs.graph,
        false,
    )
}

fn has_rule(report: &yokei::DependencyReport, rule: RuleId, name: &str) -> bool {
    report.candidates.iter().any(|candidate| {
        candidate.rule == rule
            && matches!(
                &candidate.subject,
                yokei::IssueSubject::Distribution { name: dist } if dist == name
            )
    })
}

fn candidate_for_distribution<'a>(
    report: &'a yokei::DependencyReport,
    rule: RuleId,
    name: &str,
) -> Option<&'a yokei::IssueCandidate> {
    report.candidates.iter().find(|candidate| {
        candidate.rule == rule
            && matches!(
                &candidate.subject,
                yokei::IssueSubject::Distribution { name: dist } if dist == name
            )
    })
}

#[test]
fn unused_boto3_emits_yok002() {
    let report = reconcile_fixture("unused_boto3");
    let boto3 = candidate_for_distribution(&report, RuleId::Yok002, "boto3").expect("boto3 unused");
    assert_eq!(boto3.severity, Severity::Error);
    assert_eq!(boto3.confidence, Confidence::Certain);
    assert!(!has_rule(&report, RuleId::Yok002, "requests"));
}

#[test]
fn missing_yaml_emits_yok003() {
    let report = reconcile_fixture("missing_yaml");
    let yaml = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Yok003 && candidate.message.contains("pyyaml"))
        .expect("pyyaml missing");
    assert_eq!(yaml.severity, Severity::Error);
}

#[test]
fn transitive_urllib3_emits_yok004() {
    let report = reconcile_fixture("transitive_urllib3");
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Yok004 && candidate.message.contains("urllib3"))
        .expect("urllib3 transitive");
    assert_eq!(candidate.severity, Severity::Error);
}

#[test]
fn misplaced_pytest_emits_yok005() {
    let report = reconcile_fixture("misplaced_pytest");
    let pytest =
        candidate_for_distribution(&report, RuleId::Yok005, "pytest").expect("pytest misplaced");
    assert_eq!(pytest.severity, Severity::Warning);
}

#[test]
fn unlisted_pytest_binary_emits_yok008() {
    let report = reconcile_fixture("unlisted_pytest");
    let binary = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Yok008)
        .expect("pytest binary unlisted");
    assert_eq!(binary.severity, Severity::Warning);
    assert!(binary.message.contains("pytest"));
}

#[test]
fn duplicate_requests_emits_yok009() {
    let report = reconcile_fixture("duplicate_requests");
    let duplicate = candidate_for_distribution(&report, RuleId::Yok009, "requests")
        .expect("requests duplicate");
    assert_eq!(duplicate.severity, Severity::Warning);
    assert!(duplicate.message.contains("runtime"));
    assert!(duplicate.message.contains("dev"));
}

#[test]
fn marker_pywin32_emits_yok002_likely() {
    let report = reconcile_fixture("marker_pywin32");
    let pywin32 = candidate_for_distribution(&report, RuleId::Yok002, "pywin32")
        .expect("pywin32 unused with marker");
    assert_eq!(pywin32.confidence, Confidence::Likely);
    assert_eq!(pywin32.severity, Severity::Warning);
}

#[test]
fn used_distributions_tracks_runtime_imports() {
    let report = reconcile_fixture("unused_boto3");
    assert!(report.used_distributions.contains("requests"));
    assert!(!report.used_distributions.contains("boto3"));
}

#[test]
fn reachable_import_graph_is_consistent() {
    let inputs = load_deps(&fixture("unused_boto3"), false);
    assert!(
        inputs
            .graph
            .edges()
            .iter()
            .any(|edge| matches!(edge, GraphEdge::FileImportsModule { .. }))
    );
}
