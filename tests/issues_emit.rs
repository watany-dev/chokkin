//! Integration tests for issue emission (pipeline step 12).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use chokkin::{
    Confidence, ExitStatus, ProjectRoot, RootMarker, RuleId, RuntimeOverrides, SeverityLevel,
    add_parsed_imports, analyze_reachability, analyze_symbols, apply_entry_plan,
    apply_resolution_to_graph, build_entry_roots, build_graph_skeleton, discover_project_root,
    discover_sources, emit_issues, extract_manifest, extract_plugin_hints, load_config,
    parse_project_sources, reconcile_dependencies, resolve_imports, resolve_target_version,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deps")
        .join(name)
}

struct EmitInputs {
    config: chokkin::ChokkinConfig,
    parse: chokkin::ParseSummary,
    reachability: chokkin::ReachabilityReport,
    deps: chokkin::DependencyReport,
    symbols: chokkin::SymbolReport,
    entry: chokkin::EntryPlan,
}

fn load_emit(path: &Path) -> EmitInputs {
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
    let entry = build_entry_roots(&loaded.effective, &manifest, &sources, &plugins, false)
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
        &loaded.workspace_members,
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
        false,
    )
    .expect("reachability");
    let deps = reconcile_dependencies(
        &manifest,
        &resolution,
        &reachability,
        &plugins,
        &loaded.effective,
        &sources,
        &parse,
        &graph,
        &[],
        false,
    );
    let symbols = analyze_symbols(
        &parse,
        &resolution,
        &reachability,
        &entry,
        &plugins,
        &entry.mode,
        &graph,
        &sources,
        &manifest,
    );

    EmitInputs {
        config: loaded.effective,
        parse,
        reachability,
        deps,
        symbols,
        entry,
    }
}

#[test]
fn emit_reports_unused_dependency() {
    let inputs = load_emit(&fixture("unused_boto3"));
    let report = emit_issues(
        &inputs.reachability,
        &inputs.deps,
        &inputs.symbols,
        &inputs.parse,
        &inputs.config,
        &RuntimeOverrides::default(),
        &inputs.entry.mode,
    );
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.rule == RuleId::Chk002)
    );
    assert_eq!(report.exit_status, ExitStatus::IssuesFound);
}

#[test]
fn config_ignore_suppresses_matching_issue() {
    let inputs = load_emit(&fixture("unused_boto3"));
    let mut config = inputs.config.clone();
    config
        .ignore
        .insert("CHK002".to_owned(), vec!["boto3".to_owned()]);
    let report = emit_issues(
        &inputs.reachability,
        &inputs.deps,
        &inputs.symbols,
        &inputs.parse,
        &config,
        &RuntimeOverrides::default(),
        &inputs.entry.mode,
    );
    assert!(
        report
            .issues
            .iter()
            .all(|issue| issue.rule != RuleId::Chk002)
    );
    assert!(!report.suppressed.is_empty());
}

#[test]
fn likely_unused_dependency_hidden_when_confidence_is_certain() {
    let inputs = load_emit(&fixture("marker_pywin32"));
    let mut config = inputs.config.clone();
    config.confidence = Confidence::Certain;
    let report = emit_issues(
        &inputs.reachability,
        &inputs.deps,
        &inputs.symbols,
        &inputs.parse,
        &config,
        &RuntimeOverrides::default(),
        &inputs.entry.mode,
    );
    assert!(
        report
            .issues
            .iter()
            .all(|issue| issue.rule != RuleId::Chk002)
    );
}

fn emit_with_config(inputs: &EmitInputs, config: &chokkin::ChokkinConfig) -> chokkin::IssueReport {
    emit_issues(
        &inputs.reachability,
        &inputs.deps,
        &inputs.symbols,
        &inputs.parse,
        config,
        &RuntimeOverrides::default(),
        &inputs.entry.mode,
    )
}

fn emit_with_config_and_overrides(
    inputs: &EmitInputs,
    config: &chokkin::ChokkinConfig,
    overrides: &RuntimeOverrides,
) -> chokkin::IssueReport {
    emit_issues(
        &inputs.reachability,
        &inputs.deps,
        &inputs.symbols,
        &inputs.parse,
        config,
        overrides,
        &inputs.entry.mode,
    )
}

#[test]
fn severity_off_disables_rule_without_suppressed_entry() {
    let inputs = load_emit(&fixture("unused_boto3"));
    let mut config = inputs.config.clone();
    config
        .severity
        .insert("CHK002".to_owned(), SeverityLevel::Off);
    let report = emit_with_config(&inputs, &config);
    assert!(
        report
            .issues
            .iter()
            .all(|issue| issue.rule != RuleId::Chk002)
    );
    assert!(report.suppressed.is_empty());
    assert_eq!(report.exit_status, ExitStatus::Success);
}

#[test]
fn severity_info_downgrades_exit_code_in_default_mode() {
    let inputs = load_emit(&fixture("unused_boto3"));
    let mut config = inputs.config.clone();
    config
        .severity
        .insert("CHK002".to_owned(), SeverityLevel::Info);
    let report = emit_with_config(&inputs, &config);
    let chk002 = report
        .issues
        .iter()
        .find(|issue| issue.rule == RuleId::Chk002)
        .expect("CHK002 should still be reported");
    assert_eq!(chk002.severity, chokkin::Severity::Info);
    assert_eq!(report.exit_status, ExitStatus::Success);
}

#[test]
fn severity_warning_counts_toward_exit_only_in_strict_mode() {
    let inputs = load_emit(&fixture("unused_boto3"));
    let mut config = inputs.config.clone();
    config
        .severity
        .insert("CHK002".to_owned(), SeverityLevel::Warning);
    let default_report = emit_with_config(&inputs, &config);
    let strict_report = emit_with_config_and_overrides(
        &inputs,
        &config,
        &RuntimeOverrides {
            strict: Some(true),
            ..RuntimeOverrides::default()
        },
    );
    let chk002 = default_report
        .issues
        .iter()
        .find(|issue| issue.rule == RuleId::Chk002)
        .expect("CHK002 should still be reported");
    assert_eq!(chk002.severity, chokkin::Severity::Warning);
    assert_eq!(default_report.exit_status, ExitStatus::Success);
    assert_eq!(strict_report.exit_status, ExitStatus::IssuesFound);
}

#[test]
fn severity_error_upgrade_counts_toward_exit_in_default_mode() {
    let inputs = load_emit(&fixture("unused_boto3"));
    let mut warning_config = inputs.config.clone();
    warning_config
        .severity
        .insert("CHK002".to_owned(), SeverityLevel::Warning);
    let mut error_config = warning_config.clone();
    error_config
        .severity
        .insert("CHK002".to_owned(), SeverityLevel::Error);
    let warning_report = emit_with_config(&inputs, &warning_config);
    let error_report = emit_with_config(&inputs, &error_config);
    let warning_issue = warning_report
        .issues
        .iter()
        .find(|issue| issue.rule == RuleId::Chk002)
        .expect("CHK002");
    let error_issue = error_report
        .issues
        .iter()
        .find(|issue| issue.rule == RuleId::Chk002)
        .expect("CHK002");
    assert_eq!(warning_issue.severity, chokkin::Severity::Warning);
    assert_eq!(error_issue.severity, chokkin::Severity::Error);
    assert_eq!(warning_report.exit_status, ExitStatus::Success);
    assert_eq!(error_report.exit_status, ExitStatus::IssuesFound);
}
