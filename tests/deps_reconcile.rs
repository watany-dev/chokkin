//! Integration tests for dependency reconciliation (pipeline step 10).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use chokkin::{
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
    manifest: chokkin::LoadedManifest,
    config: chokkin::ChokkinConfig,
    sources: chokkin::DiscoveredSources,
    plugins: chokkin::PluginHints,
    parse: chokkin::ParseSummary,
    graph: chokkin::ProjectGraph,
    resolution: chokkin::ResolutionIndex,
    reachability: chokkin::ReachabilityReport,
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
        let _ = graph.intern_module(reference.module.clone(), chokkin::ModuleOrigin::Unknown);
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

fn reconcile_fixture(name: &str) -> chokkin::DependencyReport {
    reconcile_fixture_with_strict(name, false)
}

fn reconcile_fixture_with_strict(name: &str, strict: bool) -> chokkin::DependencyReport {
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
        strict,
    )
}

fn has_rule(report: &chokkin::DependencyReport, rule: RuleId, name: &str) -> bool {
    report.candidates.iter().any(|candidate| {
        candidate.rule == rule
            && matches!(
                &candidate.subject,
                chokkin::IssueSubject::Distribution { name: dist } if dist == name
            )
    })
}

fn candidate_for_distribution<'a>(
    report: &'a chokkin::DependencyReport,
    rule: RuleId,
    name: &str,
) -> Option<&'a chokkin::IssueCandidate> {
    report.candidates.iter().find(|candidate| {
        candidate.rule == rule
            && matches!(
                &candidate.subject,
                chokkin::IssueSubject::Distribution { name: dist } if dist == name
            )
    })
}

#[test]
fn unused_boto3_emits_chk002() {
    let report = reconcile_fixture("unused_boto3");
    let boto3 = candidate_for_distribution(&report, RuleId::Chk002, "boto3").expect("boto3 unused");
    assert_eq!(boto3.severity, Severity::Error);
    assert_eq!(boto3.confidence, Confidence::Certain);
    assert!(!has_rule(&report, RuleId::Chk002, "requests"));
}

#[test]
fn missing_yaml_emits_chk003() {
    let report = reconcile_fixture("missing_yaml");
    let yaml = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Chk003 && candidate.message.contains("pyyaml"))
        .expect("pyyaml missing");
    assert_eq!(yaml.severity, Severity::Error);
}

#[test]
fn transitive_urllib3_emits_chk004() {
    let report = reconcile_fixture("transitive_urllib3");
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Chk004 && candidate.message.contains("urllib3"))
        .expect("urllib3 transitive");
    assert_eq!(candidate.severity, Severity::Error);
}

#[test]
fn misplaced_pytest_emits_chk005() {
    let report = reconcile_fixture("misplaced_pytest");
    let pytest =
        candidate_for_distribution(&report, RuleId::Chk005, "pytest").expect("pytest misplaced");
    assert_eq!(pytest.severity, Severity::Warning);
}

#[test]
fn unlisted_pytest_binary_emits_chk008() {
    let report = reconcile_fixture("unlisted_pytest");
    let binary = report
        .candidates
        .iter()
        .find(|candidate| candidate.rule == RuleId::Chk008)
        .expect("pytest binary unlisted");
    assert_eq!(binary.severity, Severity::Warning);
    assert!(binary.message.contains("pytest"));
}

#[test]
fn duplicate_requests_emits_chk009() {
    let report = reconcile_fixture("duplicate_requests");
    let duplicate = candidate_for_distribution(&report, RuleId::Chk009, "requests")
        .expect("requests duplicate");
    assert_eq!(duplicate.severity, Severity::Warning);
    assert!(duplicate.message.contains("runtime"));
    assert!(duplicate.message.contains("dev"));
}

#[test]
fn marker_pywin32_emits_chk002_likely_in_strict_mode() {
    let report = reconcile_fixture_with_strict("marker_pywin32", true);
    let pywin32 = candidate_for_distribution(&report, RuleId::Chk002, "pywin32")
        .expect("pywin32 unused with marker");
    assert_eq!(pywin32.confidence, Confidence::Likely);
    assert_eq!(pywin32.severity, Severity::Error);
}

#[test]
fn marker_pywin32_suppressed_by_default() {
    let report = reconcile_fixture("marker_pywin32");
    assert!(!has_rule(&report, RuleId::Chk002, "pywin32"));
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

#[test]
fn map_alias_import_resolves_to_python_multipart() {
    let report = reconcile_fixture("map_alias");
    assert!(!has_rule(&report, RuleId::Chk002, "python-multipart"));
    assert!(report.used_distributions.contains("python-multipart"));
}

#[test]
fn self_extra_dependency_is_not_unused() {
    let report = reconcile_fixture("self_extra");
    assert!(!has_rule(&report, RuleId::Chk002, "self-extra"));
    assert!(report.used_distributions.contains("self-extra"));
}

#[test]
fn binary_tool_pyproject_marks_dev_tools_used() {
    let report = reconcile_fixture("binary_tool_pyproject");
    assert!(!has_rule(&report, RuleId::Chk002, "mypy"));
    assert!(!has_rule(&report, RuleId::Chk002, "ruff"));
    assert!(report.used_distributions.contains("mypy"));
    assert!(report.used_distributions.contains("ruff"));
}

#[test]
fn binary_mkdocs_theme_marks_material_used() {
    let report = reconcile_fixture("binary_mkdocs_theme");
    assert!(!has_rule(&report, RuleId::Chk002, "mkdocs"));
    assert!(!has_rule(&report, RuleId::Chk002, "mkdocs-material"));
    assert!(report.used_distributions.contains("mkdocs"));
    assert!(report.used_distributions.contains("mkdocs-material"));
}

#[test]
fn dev_group_only_suppresses_chk002_for_pytest() {
    let report = reconcile_fixture("dev_group_only");
    assert!(!has_rule(&report, RuleId::Chk002, "pytest"));
}

#[test]
fn pdm_dev_dependencies_suppress_chk002() {
    let report = reconcile_fixture("pdm_dev_deps");
    assert!(!has_rule(&report, RuleId::Chk002, "pytest"));
}

#[test]
fn optional_try_import_marks_brotli_used() {
    let report = reconcile_fixture("optional_try_import");
    assert!(!has_rule(&report, RuleId::Chk002, "brotli"));
    assert!(report.used_distributions.contains("brotli"));
}

#[test]
fn platform_guard_import_marks_tzdata_used() {
    let report = reconcile_fixture("platform_guard_import");
    assert!(!has_rule(&report, RuleId::Chk002, "tzdata"));
    assert!(report.used_distributions.contains("tzdata"));
}
