//! Integration tests for issue emission (pipeline step 12).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chokkin::internals::analyze_with_context;
use chokkin::internals::reconcile_with_context;
use chokkin::internals::{
    Confidence, ModuleOrigin, PluginExtractRequest, ProjectRoot, ResolutionIndex, RootMarker,
    RuleId, ScopedDeclarations, SeverityLevel, StdlibRange, TargetVersion, add_parsed_imports,
    analyze_reachability, apply_resolution_to_graph, build_entry_roots, build_graph_skeleton,
    collect_gaps, discover_project_root, discover_sources, emit_issues, extract_manifest,
    extract_plugin_hints_with_parse, load_config, parse_project_sources_with_cache,
    resolve_imports_for_analysis, resolve_target_version,
};
use chokkin::internals::{DependencyRuleContext, RuleContext};
use chokkin::{ExitStatus, RuntimeOverrides};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deps")
        .join(name)
}

struct EmitInputs {
    config: chokkin::internals::ChokkinConfig,
    parse: chokkin::internals::ParseSummary,
    reachability: chokkin::internals::ReachabilityReport,
    deps: chokkin::internals::DependencyReport,
    symbols: Vec<chokkin::internals::IssueCandidate>,
    gaps: Vec<chokkin::internals::Gap>,
}

fn load_emit(path: &Path) -> EmitInputs {
    load_emit_with_strict_deps(path, false)
}

/// `strict_deps` only affects dependency reconciliation, so step 12 can still
/// apply the configured confidence floor to candidates that non-strict step 10
/// would never produce (e.g. marker-guarded dependencies).
fn load_emit_with_strict_deps(path: &Path, strict_deps: bool) -> EmitInputs {
    let root = discover_project_root(path).unwrap_or_else(|_| ProjectRoot {
        path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        marker: RootMarker::PyProjectToml,
    });
    let loaded = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &loaded).expect("extract manifest");
    let sources = discover_sources(&root, &loaded, &manifest).expect("discover sources");
    let target = resolve_target_version(&loaded.effective, &manifest);
    let parse = parse_project_sources_with_cache(&root, &sources, &target, None).expect("parse");
    let plugins = extract_plugin_hints_with_parse(&PluginExtractRequest {
        root: &root,
        config: &loaded,
        sources: &sources,
        manifest: &manifest,
        parse: &parse,
        cache: None,
    })
    .expect("plugin hints");
    let entry = build_entry_roots(&loaded.effective, &manifest, &sources, &plugins, false);

    let mut graph = build_graph_skeleton(&manifest, &sources).expect("graph skeleton");
    for module in &parse.modules {
        let file_id = graph.file_id(&module.path).expect("file id");
        add_parsed_imports(&mut graph, file_id, module).expect("parsed imports");
    }
    let plugin_refs: Vec<_> = plugins.module_refs().cloned().collect();
    for reference in &plugin_refs {
        let _ = graph.intern_module(reference.module.clone(), ModuleOrigin::Unknown);
    }
    let resolution = resolve_imports_for_analysis(
        &loaded.effective,
        &manifest,
        &sources,
        &parse,
        &plugin_refs,
        &[],
        &loaded.workspace_members,
        &BTreeMap::new(),
        &ScopedDeclarations::default(),
    );
    apply_resolution_to_graph(&mut graph, &resolution).expect("apply resolution");
    let reachability = analyze_reachability(&mut graph, &sources, &entry, &plugins, &parse, false)
        .expect("reachability");
    let rules = RuleContext {
        resolution: &resolution,
        reachability: &reachability,
        graph: &graph,
        sources: &sources,
        parse: &parse,
    };
    let deps = reconcile_with_context(
        &DependencyRuleContext {
            rules: &rules,
            config: &loaded.effective,
            strict: strict_deps,
            build_scripts: &BTreeMap::new(),
        },
        &manifest,
        &plugins,
        &[],
        &[],
    );
    let symbols = analyze_with_context(&rules, &entry, &plugins, &manifest, &[], None);
    let gaps = collect_gaps(
        &rules,
        &manifest,
        &[],
        &[],
        &plugins,
        &[],
        &loaded.effective.dependencies,
        StdlibRange::new(&TargetVersion::default_py311(), None),
    );

    EmitInputs {
        config: loaded.effective,
        parse,
        reachability,
        deps,
        symbols,
        gaps,
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
        &inputs.gaps,
        &inputs.config,
        &RuntimeOverrides::default(),
        &ResolutionIndex::default(),
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
        &inputs.gaps,
        &config,
        &RuntimeOverrides::default(),
        &ResolutionIndex::default(),
    );
    assert!(
        report
            .issues
            .iter()
            .all(|issue| issue.rule != RuleId::Chk002)
    );
    assert_ne!(report.suppressed, []);
}

/// An unreadable `install_requires` may declare any import, so CHK003 is an
/// info hint that names the gap (#491, ADR 0005).
#[test]
fn missing_is_a_hint_naming_the_gap_when_runtime_dependencies_are_unknown() {
    let inputs = load_emit(&fixture("setup_py_unknown_runtime"));
    let mut config = inputs.config.clone();
    config.confidence = Confidence::Maybe;
    let report = emit_with_config(&inputs, &config);
    let yaml = report
        .issues
        .iter()
        .find(|issue| issue.rule == RuleId::Chk003 && issue.message.contains("pyyaml"))
        .expect("pyyaml missing");
    assert_eq!(yaml.severity, chokkin::internals::Severity::Info);
    assert_eq!(yaml.confidence, Confidence::Maybe);
    let details = &yaml.explain.as_ref().expect("explain").details;
    assert!(
        details
            .iter()
            .any(|line| line == "runtime dependencies could not be read statically: `setup.py`"),
        "{details:?}"
    );
}

/// An `extras_require` the evaluator cannot read may declare any import as
/// optional, so CHK003 drops below certain and names the gap (#975, ADR
/// 0005 §6). Runtime dependencies were read, so it stays an error.
#[test]
fn missing_is_likely_naming_the_gap_when_extras_are_unknown() {
    let inputs = load_emit(&fixture("setup_py_unknown_extras"));
    let mut config = inputs.config.clone();
    config.confidence = Confidence::Maybe;
    let report = emit_with_config(&inputs, &config);
    let yaml = report
        .issues
        .iter()
        .find(|issue| issue.rule == RuleId::Chk003 && issue.message.contains("pyyaml"))
        .expect("pyyaml missing");
    assert_eq!(yaml.severity, chokkin::internals::Severity::Error);
    assert_eq!(yaml.confidence, Confidence::Likely);
    let details = &yaml.explain.as_ref().expect("explain").details;
    assert!(
        details
            .iter()
            .any(|line| line
                == "extras could not be read statically: `extras_require` in `setup.py`"),
        "{details:?}"
    );
}

#[test]
fn likely_unused_dependency_hidden_when_confidence_is_certain() {
    let inputs = load_emit_with_strict_deps(&fixture("marker_pywin32"), true);
    let is_likely_pywin32 = |issue: &chokkin::internals::Issue| {
        issue.rule == RuleId::Chk002
            && issue.confidence == Confidence::Likely
            && matches!(
                &issue.subject,
                chokkin::internals::IssueSubject::Distribution { name } if name == "pywin32"
            )
    };

    let baseline = emit_with_config(&inputs, &inputs.config);
    assert!(baseline.issues.iter().any(is_likely_pywin32));

    let mut config = inputs.config.clone();
    config.confidence = Confidence::Certain;
    let report = emit_with_config(&inputs, &config);
    assert!(!report.issues.iter().any(is_likely_pywin32));
}

fn emit_with_config(
    inputs: &EmitInputs,
    config: &chokkin::internals::ChokkinConfig,
) -> chokkin::internals::IssueReport {
    emit_issues(
        &inputs.reachability,
        &inputs.deps,
        &inputs.symbols,
        &inputs.parse,
        &inputs.gaps,
        config,
        &RuntimeOverrides::default(),
        &ResolutionIndex::default(),
    )
}

fn emit_with_config_and_overrides(
    inputs: &EmitInputs,
    config: &chokkin::internals::ChokkinConfig,
    overrides: &RuntimeOverrides,
) -> chokkin::internals::IssueReport {
    emit_issues(
        &inputs.reachability,
        &inputs.deps,
        &inputs.symbols,
        &inputs.parse,
        &inputs.gaps,
        config,
        overrides,
        &ResolutionIndex::default(),
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
    assert_eq!(report.suppressed, []);
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
    assert_eq!(chk002.severity, chokkin::internals::Severity::Info);
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
    assert_eq!(chk002.severity, chokkin::internals::Severity::Warning);
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
    assert_eq!(
        warning_issue.severity,
        chokkin::internals::Severity::Warning
    );
    assert_eq!(error_issue.severity, chokkin::internals::Severity::Error);
    assert_eq!(warning_report.exit_status, ExitStatus::Success);
    assert_eq!(error_report.exit_status, ExitStatus::IssuesFound);
}
