//! Full project analysis orchestration (pipeline steps 1–13).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::baseline::{BaselineReport, apply_baseline, write_baseline};
use crate::cache::CacheOptions;
use crate::config::{ProjectMode, RuntimeOverrides};
use crate::entry::{EntryPlan, add_member_manifest_roots, build_entry_roots, is_library_member};
use crate::fix::{FixOptions, FixReport, WorkspaceFixManifest, apply_fixes_with_workspace};
use crate::graph::{GraphError, ProjectGraph, add_parsed_imports, build_graph_skeleton};
use crate::manifest::{DeclaredDependency, normalize_distribution_name};
use crate::parser::parse_project_sources_with_cache;
use crate::plugins::{PluginExtractRequest, extract_plugin_hints_with_parse};
use crate::reachability::{
    ReachabilityReport, analyze_reachability, apply_member_surfaces, apply_public_surface,
};
use crate::reporters::FileCounts;
use crate::resolver::{
    ScopedDeclarations, StdlibRange, apply_resolution_to_graph, resolve_imports_for_analysis,
};
use crate::rules::{
    DependencyRuleContext, IssueReport, RuleContext, WorkspaceDependencyBoundary, emit_issues,
};
use crate::sources::{DiscoveredSources, FileContext, PublicSurface, discover_sources};

use super::error::AnalyzeError;
use super::probe::{ProbeReport, probe_project_with_cache};
use super::warnings::ProbeWarning;

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
    /// Final issue report (step 12).
    pub issues: IssueReport,
    /// Fix report when `--fix` was requested (step 13).
    pub fix: Option<FixReport>,
    /// Baseline report when `--baseline` was requested.
    pub baseline: Option<BaselineReport>,
    /// Non-fatal warnings from the full analysis pipeline.
    pub warnings: Vec<ProbeWarning>,
}

impl AnalysisReport {
    /// Counts runtime-context files in the graph and how many are reachable.
    #[must_use]
    pub fn runtime_file_counts(&self) -> FileCounts {
        let mut counts = FileCounts::default();
        for (id, file) in self.graph.files() {
            if file.context == FileContext::Runtime {
                counts.runtime += 1;
                if self.reachability.reachable.contains(&id) {
                    counts.reachable_runtime += 1;
                }
            }
        }
        counts
    }
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
/// Returns `AnalyzeError` when a pipeline step fails fatally.
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
    let baseline = apply_baseline_options(&mut core.issues, &probe.root.path, overrides, &options)?;
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
        ))
    } else {
        None
    };

    Ok(AnalysisReport {
        probe,
        graph: core.graph,
        reachability: core.reachability,
        entry: core.entry,
        issues: core.issues,
        fix,
        baseline,
        warnings: core.warnings,
    })
}

fn apply_baseline_options(
    issues: &mut IssueReport,
    root: &Path,
    overrides: &RuntimeOverrides,
    options: &AnalyzeOptions,
) -> Result<Option<BaselineReport>, AnalyzeError> {
    let Some(path) = &options.baseline else {
        return Ok(None);
    };
    if options.update_baseline {
        return Ok(Some(write_baseline(issues, root, path)?));
    }
    Ok(Some(apply_baseline(issues, root, path, overrides)?))
}

struct AnalysisCore {
    graph: ProjectGraph,
    reachability: ReachabilityReport,
    entry: EntryPlan,
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
        auto_workspace: probe.auto_workspace,
    };

    let target = probe
        .effective_config
        .target_version
        .clone()
        .unwrap_or_else(crate::config::TargetVersion::default_py311);

    let parse = parse_project_sources_with_cache(
        &probe.root,
        &probe.sources,
        &target,
        Some(&options.cache),
    )?;

    // Step 5 runs after step 6 so Flask and Celery can read decorators off the
    // parse output instead of re-opening every source file. Nothing in parse
    // depends on plugin hints.
    let plugins = extract_plugin_hints_with_parse(&PluginExtractRequest {
        root: &probe.root,
        config: &loaded,
        sources: &probe.sources,
        manifest: &probe.manifest,
        parse: &parse,
        cache: Some(&options.cache),
    })?;
    let warnings: Vec<ProbeWarning> = parse
        .modules
        .iter()
        .filter(|module| module.skipped)
        .map(|module| ProbeWarning::SkippedSource {
            path: module.path.clone(),
        })
        .chain(plugins.warnings.iter().cloned().map(ProbeWarning::Plugin))
        .collect();

    let mut entry = build_entry_roots(
        &probe.effective_config,
        &probe.manifest,
        &probe.sources,
        &plugins,
        production,
    );
    crate::entry::add_script_roots(&mut entry, &probe.scripts, &probe.sources, production);
    add_member_manifest_roots(
        &mut entry,
        &probe.sources,
        probe
            .workspace_inputs
            .iter()
            .map(|input| (input.member.path.as_str(), &input.manifest, &input.sources)),
    );
    if probe.effective_config.mode == ProjectMode::Auto {
        entry.library_members = probe
            .workspace_inputs
            .iter()
            .filter(|input| is_library_member(&input.manifest, &input.sources))
            .map(|input| input.member.path.clone())
            .collect();
    }

    let mut graph = build_analysis_graph(probe, &parse, &plugins)?;

    let plugin_refs: Vec<_> = plugins.module_refs().cloned().collect();
    let script_targets: BTreeMap<_, _> = probe
        .scripts
        .iter()
        .filter_map(|script| {
            let target = script.target_version.as_ref()?;
            let range = StdlibRange::new(target, script.requires_python.as_deref());
            Some((script.path.clone(), range))
        })
        .collect();
    let resolution = resolve_imports_for_analysis(
        &probe.effective_config,
        &probe.manifest,
        &probe.sources,
        &parse,
        &plugin_refs,
        &probe.workspace_members,
        &script_targets,
        &scoped_declarations(probe),
    );
    apply_resolution_to_graph(&mut graph, &resolution)?;

    let mut reachability = analyze_reachability(
        &mut graph,
        &probe.sources,
        &entry,
        &plugins,
        &parse,
        production,
    )?;
    if let Some(surface) = PublicSurface::resolve(
        probe.manifest.metadata.wheel_targets.as_ref(),
        &probe.sources.files,
    ) {
        apply_public_surface(&mut reachability, &surface, &entry);
    }
    // CHK007 needs every member's surface, app members too (#678). A member
    // without wheel targets ships nothing, so its surface is empty.
    let member_surfaces: Vec<_> = probe
        .workspace_inputs
        .iter()
        .map(|input| {
            let surface = PublicSurface::resolve(
                input.manifest.metadata.wheel_targets.as_ref(),
                &input.sources.files,
            )
            .unwrap_or_default();
            (input.member.path.clone(), surface)
        })
        .collect();
    let library_surfaces: Vec<_> = member_surfaces
        .iter()
        .filter(|(member, surface)| {
            !surface.files.is_empty() && entry.library_members.contains(member)
        })
        .collect();
    apply_member_surfaces(&mut reachability, &library_surfaces);

    let workspace_boundaries = probe
        .workspace_inputs
        .iter()
        .map(|input| WorkspaceDependencyBoundary {
            member_id: &input.member.id,
            manifest: &input.manifest,
            files: &input.sources.files,
        })
        .collect::<Vec<_>>();

    let context = RuleContext {
        resolution: &resolution,
        reachability: &reachability,
        graph: &graph,
        sources: &probe.sources,
        parse: &parse,
    };
    let deps = crate::rules::deps::reconcile_with_context(
        &DependencyRuleContext {
            rules: &context,
            config: &probe.effective_config,
            strict,
        },
        &probe.manifest,
        &plugins,
        &workspace_boundaries,
        &probe.scripts,
    );

    let production_tests = if production
        && (entry.mode == ProjectMode::Library || !entry.library_members.is_empty())
    {
        Some(parse_production_tests(probe, &loaded, &target)?)
    } else {
        None
    };
    let symbols = crate::rules::symbols::analyze_with_context(
        &context,
        &entry,
        &plugins,
        &probe.manifest,
        &member_surfaces,
        production_tests.as_deref(),
    );

    let issues = emit_issues(
        &reachability,
        &deps,
        &symbols,
        &parse,
        &probe.effective_config,
        overrides,
        &resolution,
    );

    Ok(AnalysisCore {
        graph,
        reachability,
        entry,
        issues,
        warnings,
    })
}

/// `--production` drops tests from discovery, but a library's tests are still
/// the evidence that a public module's name is called from outside (#588).
fn parse_production_tests(
    probe: &ProbeReport,
    loaded: &crate::config::LoadedConfig,
    target: &crate::config::TargetVersion,
) -> Result<Vec<crate::parser::ParsedModule>, AnalyzeError> {
    let mut config = loaded.clone();
    config.effective.production = false;
    let discovered = discover_sources(&probe.root, &config, &probe.manifest)
        .map_err(super::error::ProbeError::from)?;
    let tests = DiscoveredSources {
        files: discovered
            .files
            .into_iter()
            .filter(|file| file.context == FileContext::Test)
            .collect(),
        ..probe.sources.clone()
    };
    Ok(parse_project_sources_with_cache(&probe.root, &tests, target, None)?.modules)
}

fn scoped_declarations(probe: &ProbeReport) -> ScopedDeclarations {
    let names = |dependencies: &[DeclaredDependency]| {
        dependencies
            .iter()
            .map(|dep| normalize_distribution_name(&dep.name))
            .collect::<BTreeSet<_>>()
    };
    ScopedDeclarations {
        scripts: probe
            .scripts
            .iter()
            .map(|script| (script.path.clone(), names(&script.dependencies)))
            .collect(),
        members: probe
            .workspace_inputs
            .iter()
            .map(|input| (input.member.id.clone(), names(&input.manifest.dependencies)))
            .collect(),
        member_locks: probe
            .workspace_inputs
            .iter()
            .map(|input| {
                let locked = input.manifest.lockfile.edges.keys().cloned().collect();
                (input.member.id.clone(), locked)
            })
            .collect(),
    }
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
            .ok_or_else(|| GraphError::Invariant {
                detail: format!("unknown parsed file `{}`", module.path),
            })?;
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
    use crate::rules::IssueSubject;
    use crate::rules::RuleId;
    use crate::rules::Severity;

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
        assert_eq!(report.issues.issues, []);
    }

    #[test]
    fn runtime_file_counts_skip_tests_and_count_reachable_files() {
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path();
        fs::write(
            root.join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.0.0\"\n\n[project.scripts]\ndemo = \"demo.cli:main\"\n",
        )
        .expect("write");
        fs::create_dir_all(root.join("demo")).expect("mkdir");
        fs::create_dir_all(root.join("tests")).expect("mkdir");
        fs::write(root.join("demo/__init__.py"), "").expect("write");
        fs::write(
            root.join("demo/cli.py"),
            "from demo import used\n\ndef main():\n    used.run()\n",
        )
        .expect("write");
        fs::write(root.join("demo/used.py"), "def run():\n    pass\n").expect("write");
        fs::write(root.join("demo/orphan.py"), "X = 1\n").expect("write");
        fs::write(root.join("tests/test_demo.py"), "import demo.used\n").expect("write");

        let report = analyze_project(
            root,
            None,
            &RuntimeOverrides::default(),
            AnalyzeOptions::default(),
        )
        .expect("analyze");
        assert_eq!(
            report.runtime_file_counts(),
            FileCounts {
                runtime: 4,
                reachable_runtime: 3,
            }
        );
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
