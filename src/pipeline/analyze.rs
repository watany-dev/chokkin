//! Full project analysis orchestration (pipeline steps 1–13).

use std::path::Path;

use crate::baseline::{BaselineReport, apply_baseline, write_baseline};
use crate::cache::{CacheOptions, ParseCacheStore};
use crate::config::RuntimeOverrides;
use crate::entry::{EntryPlan, ResolvedMode, apply_entry_plan, build_entry_roots};
use crate::fix::{FixOptions, FixReport, WorkspaceFixManifest, apply_fixes_with_workspace};
use crate::graph::{ProjectGraph, add_parsed_imports, build_graph_skeleton};
use crate::parser::parse_project_sources_with_cache;
use crate::plugins::extract_plugin_hints_with_cache;
use crate::reachability::{ReachabilityReport, analyze_reachability_with_cache};
use crate::resolver::{apply_resolution_to_graph, resolve_imports};
use crate::rules::{
    IssueReport, WorkspaceDependencyBoundary, analyze_symbols, emit_issues, reconcile_dependencies,
};

use super::error::AnalyzeError;
use super::probe::{ProbeReport, probe_project_with_cache};
use super::warnings::{ProbeWarning, actionable_plugin_warnings};

/// Outcome of running the full analysis pipeline (steps 1–12, optional 13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisReport {
    /// Steps 1–4 probe summary.
    pub probe: ProbeReport,
    /// Project graph after parse, resolution, and entry wiring.
    pub graph: ProjectGraph,
    /// Reachability analysis output (step 9).
    pub reachability: ReachabilityReport,
    /// Entry root plan used for reachability (step 8).
    pub entry: EntryPlan,
    /// Resolved project mode from entry construction (step 8).
    pub entry_mode: ResolvedMode,
    /// Final issue report (step 12).
    pub issues: IssueReport,
    /// Fix report when `--fix` was requested (step 13).
    pub fix: Option<FixReport>,
    /// Baseline report when `--baseline` was requested.
    pub baseline: Option<BaselineReport>,
    /// Non-fatal warnings from the full analysis pipeline.
    pub warnings: Vec<ProbeWarning>,
}

/// Options for the analysis run beyond [`RuntimeOverrides`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnalyzeOptions {
    /// When true, run step 13 after issue emission.
    pub fix_enabled: bool,
    /// Fix behaviour when `fix_enabled` is true.
    pub fix: FixOptions,
    /// Baseline file to read/filter after issue emission.
    pub baseline: Option<std::path::PathBuf>,
    /// Update the baseline file with the current issue set.
    pub update_baseline: bool,
    /// Cache policy for Phase 2 warm-run support.
    pub cache: CacheOptions,
}

/// Run pipeline steps 1–12 and optionally step 13.
///
/// # Errors
///
/// Returns [`AnalyzeError`] when a pipeline step fails fatally.
#[allow(clippy::needless_pass_by_value)]
pub fn analyze_project(
    start: &Path,
    project_root_override: Option<&Path>,
    overrides: &RuntimeOverrides,
    options: AnalyzeOptions,
) -> Result<AnalysisReport, AnalyzeError> {
    let probe = probe_project_with_cache(
        start,
        project_root_override,
        overrides,
        Some(&options.cache),
    )?;
    let mut core = run_analysis_core(&probe, overrides, &options)?;
    let baseline = apply_baseline_options(&mut core.issues, &probe.root.path, &options)?;
    let fix = if options.fix_enabled {
        let workspace_manifests = probe
            .workspace_inputs
            .iter()
            .map(|input| WorkspaceFixManifest {
                id: input.member.id.as_str(),
                path: input.member.path.as_str(),
                pyproject_toml: input.member.pyproject_toml.as_deref(),
                manifest: &input.manifest,
            })
            .collect::<Vec<_>>();
        Some(apply_fixes_with_workspace(
            &core.issues,
            &probe.root,
            &probe.manifest,
            &workspace_manifests,
            options.fix,
        )?)
    } else {
        None
    };

    Ok(AnalysisReport {
        probe,
        graph: core.graph,
        reachability: core.reachability,
        entry: core.entry,
        entry_mode: core.entry_mode,
        issues: core.issues,
        fix,
        baseline,
        warnings: core.warnings,
    })
}

fn apply_baseline_options(
    issues: &mut IssueReport,
    root: &Path,
    options: &AnalyzeOptions,
) -> Result<Option<BaselineReport>, AnalyzeError> {
    let Some(path) = &options.baseline else {
        return Ok(None);
    };
    if options.update_baseline {
        return Ok(Some(write_baseline(issues, root, path)?));
    }
    Ok(Some(apply_baseline(issues, root, path)?))
}

struct AnalysisCore {
    graph: ProjectGraph,
    reachability: ReachabilityReport,
    entry: EntryPlan,
    entry_mode: ResolvedMode,
    issues: IssueReport,
    warnings: Vec<ProbeWarning>,
}

#[allow(clippy::too_many_lines)]
fn run_analysis_core(
    probe: &ProbeReport,
    overrides: &RuntimeOverrides,
    options: &AnalyzeOptions,
) -> Result<AnalysisCore, AnalyzeError> {
    let production = probe.effective_config.production;
    let strict = overrides.strict.unwrap_or(false);
    let loaded = crate::config::LoadedConfig {
        root: probe.root.clone(),
        effective: probe.effective_config.clone(),
        sources: probe.config_sources.clone(),
        uv_workspace: probe.manifest.uv_workspace.clone(),
        workspace_members: probe.workspace_members.clone(),
    };

    let plugins = extract_plugin_hints_with_cache(
        &probe.root,
        &loaded,
        &probe.sources,
        &probe.manifest,
        Some(&options.cache),
    )?;
    let warnings = actionable_plugin_warnings(&plugins);

    let target = probe
        .effective_config
        .target_version
        .clone()
        .unwrap_or_else(crate::config::TargetVersion::default_py311);

    let mut parse_cache = options.cache.enabled.then(ParseCacheStore::new);
    let parse = parse_project_sources_with_cache(
        &probe.root,
        &probe.sources,
        &target,
        parse_cache.as_mut(),
        Some(&options.cache),
    )?;

    let entry = build_entry_roots(
        &probe.effective_config,
        &probe.manifest,
        &probe.sources,
        &plugins,
        production,
    )?;

    let mut graph = build_analysis_graph(probe, &parse, &plugins)?;

    let plugin_refs: Vec<_> = plugins.module_refs().cloned().collect();
    let resolution = resolve_imports(
        &probe.root,
        &probe.effective_config,
        &probe.manifest,
        &probe.sources,
        &parse,
        &plugin_refs,
        &probe.workspace_members,
    )?;
    apply_resolution_to_graph(&mut graph, &resolution)?;
    apply_entry_plan(&mut graph, &entry)?;

    let reachability = analyze_reachability_with_cache(
        &mut graph,
        &probe.sources,
        &entry,
        &plugins,
        &parse,
        &entry.mode,
        production,
        Some(&options.cache),
    )?;

    let workspace_boundaries = probe
        .workspace_inputs
        .iter()
        .map(|input| WorkspaceDependencyBoundary {
            member_id: &input.member.id,
            manifest: &input.manifest,
        })
        .collect::<Vec<_>>();

    let deps = reconcile_dependencies(
        &probe.manifest,
        &resolution,
        &reachability,
        &plugins,
        &probe.effective_config,
        &probe.sources,
        &parse,
        &graph,
        &workspace_boundaries,
        strict,
    );

    let symbols = analyze_symbols(
        &parse,
        &resolution,
        &reachability,
        &entry,
        &plugins,
        &entry.mode,
        &graph,
        &probe.sources,
        &probe.manifest,
    );

    let issues = emit_issues(
        &reachability,
        &deps,
        &symbols,
        &parse,
        &probe.effective_config,
        overrides,
        &entry.mode,
    );

    let entry_mode = entry.mode.clone();

    Ok(AnalysisCore {
        graph,
        reachability,
        entry,
        entry_mode,
        issues,
        warnings,
    })
}

fn build_analysis_graph(
    probe: &ProbeReport,
    parse: &crate::parser::ParseSummary,
    plugins: &crate::plugins::PluginHints,
) -> Result<ProjectGraph, AnalyzeError> {
    let mut graph = build_graph_skeleton(&probe.manifest, &probe.sources)?;
    for module in &parse.modules {
        let file_id = graph
            .file_id(&module.path)
            .ok_or_else(|| AnalyzeError::Usage(format!("unknown parsed file `{}`", module.path)))?;
        add_parsed_imports(&mut graph, file_id, module)?;
    }
    for reference in plugins.module_refs() {
        let _ = graph.intern_module(
            reference.module.clone(),
            crate::graph::ModuleOrigin::Unknown,
        );
    }
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;
    use crate::ExitStatus;
    use crate::RuleId;
    use crate::Severity;
    use crate::rules::IssueSubject;

    #[test]
    fn analyze_unused_dependency_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deps/unused_boto3");
        let report = analyze_project(
            &root,
            None,
            &RuntimeOverrides::default(),
            AnalyzeOptions::default(),
        )
        .expect("analyze");
        assert!(
            report
                .issues
                .issues
                .iter()
                .any(|issue| issue.rule == RuleId::Chk002)
        );
        assert_eq!(report.issues.exit_status, ExitStatus::IssuesFound);
    }

    #[test]
    fn analyze_empty_project_succeeds() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("pyproject.toml"),
            "[project]\nname = \"empty\"\nversion = \"0.0.0\"\n",
        )
        .expect("write");

        let report = analyze_project(
            temp.path(),
            None,
            &RuntimeOverrides::default(),
            AnalyzeOptions::default(),
        )
        .expect("analyze");
        assert!(report.issues.issues.is_empty());
    }

    #[test]
    fn analyze_strict_passes_strict_to_dependency_reconciliation() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deps/marker_pywin32");
        let default_report = analyze_project(
            &root,
            None,
            &RuntimeOverrides::default(),
            AnalyzeOptions::default(),
        )
        .expect("analyze");
        assert!(!default_report.issues.issues.iter().any(|issue| {
            issue.rule == RuleId::Chk002
                && matches!(
                    &issue.subject,
                    IssueSubject::Distribution { name } if name == "pywin32"
                )
        }));

        let strict_report = analyze_project(
            &root,
            None,
            &RuntimeOverrides {
                strict: Some(true),
                ..RuntimeOverrides::default()
            },
            AnalyzeOptions::default(),
        )
        .expect("analyze");
        let pywin32 = strict_report
            .issues
            .issues
            .iter()
            .find(|issue| {
                issue.rule == RuleId::Chk002
                    && matches!(
                        &issue.subject,
                        IssueSubject::Distribution { name } if name == "pywin32"
                    )
            })
            .expect("pywin32 CHK002 in strict mode");
        assert_eq!(pywin32.severity, Severity::Error);
    }
}
