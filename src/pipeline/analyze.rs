//! Full project analysis orchestration (pipeline steps 1–13).

use std::path::Path;

use crate::baseline::{BaselineReport, apply_baseline, write_baseline};
use crate::config::RuntimeOverrides;
use crate::entry::{ResolvedMode, apply_entry_plan, build_entry_roots};
use crate::fix::{FixOptions, FixReport, apply_fixes};
use crate::graph::{ProjectGraph, add_parsed_imports, build_graph_skeleton};
use crate::parser::parse_project_sources;
use crate::plugins::extract_plugin_hints;
use crate::reachability::{ReachabilityReport, analyze_reachability};
use crate::resolver::{apply_resolution_to_graph, resolve_imports};
use crate::rules::{IssueReport, analyze_symbols, emit_issues, reconcile_dependencies};

use super::error::AnalyzeError;
use super::probe::{ProbeReport, probe_project};

/// Outcome of running the full analysis pipeline (steps 1–12, optional 13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisReport {
    /// Steps 1–4 probe summary.
    pub probe: ProbeReport,
    /// Project graph after parse, resolution, and entry wiring.
    pub graph: ProjectGraph,
    /// Reachability analysis output (step 9).
    pub reachability: ReachabilityReport,
    /// Resolved project mode from entry construction (step 8).
    pub entry_mode: ResolvedMode,
    /// Final issue report (step 12).
    pub issues: IssueReport,
    /// Fix report when `--fix` was requested (step 13).
    pub fix: Option<FixReport>,
    /// Baseline report when `--baseline` was requested.
    pub baseline: Option<BaselineReport>,
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
}

/// Run pipeline steps 1–12 and optionally step 13.
///
/// # Errors
///
/// Returns [`AnalyzeError`] when a pipeline step fails fatally.
pub fn analyze_project(
    start: &Path,
    project_root_override: Option<&Path>,
    overrides: &RuntimeOverrides,
    options: AnalyzeOptions,
) -> Result<AnalysisReport, AnalyzeError> {
    let probe = probe_project(start, project_root_override, overrides)?;
    let mut core = run_analysis_core(&probe, overrides)?;
    let baseline = apply_baseline_options(&mut core.issues, &probe.root.path, &options)?;
    let fix = if options.fix_enabled {
        Some(apply_fixes(
            &core.issues,
            &probe.root,
            &probe.manifest,
            options.fix,
        )?)
    } else {
        None
    };

    Ok(AnalysisReport {
        probe,
        graph: core.graph,
        reachability: core.reachability,
        entry_mode: core.entry_mode,
        issues: core.issues,
        fix,
        baseline,
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
    entry_mode: ResolvedMode,
    issues: IssueReport,
}

fn run_analysis_core(
    probe: &ProbeReport,
    overrides: &RuntimeOverrides,
) -> Result<AnalysisCore, AnalyzeError> {
    let production = probe.effective_config.production;
    let loaded = crate::config::LoadedConfig {
        root: probe.root.clone(),
        effective: probe.effective_config.clone(),
        sources: probe.config_sources.clone(),
        uv_workspace: None,
        workspace_members: Vec::new(),
    };

    let plugins = extract_plugin_hints(&probe.root, &loaded, &probe.sources, &probe.manifest)?;

    let target = probe
        .effective_config
        .target_version
        .clone()
        .unwrap_or_else(crate::config::TargetVersion::default_py311);

    let parse = parse_project_sources(&probe.root, &probe.sources, &target)?;

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
    )?;
    apply_resolution_to_graph(&mut graph, &resolution)?;
    apply_entry_plan(&mut graph, &entry)?;

    let reachability = analyze_reachability(
        &mut graph,
        &probe.sources,
        &entry,
        &plugins,
        &parse,
        &entry.mode,
        production,
    )?;

    let deps = reconcile_dependencies(
        &probe.manifest,
        &resolution,
        &reachability,
        &plugins,
        &probe.effective_config,
        &probe.sources,
        &parse,
        &graph,
        production,
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

    Ok(AnalysisCore {
        graph,
        reachability,
        entry_mode: entry.mode,
        issues,
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
}
