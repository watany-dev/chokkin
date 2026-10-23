//! Full project analysis orchestration (pipeline steps 1–13).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use crate::ExitStatus;
use crate::baseline::{BaselineReport, apply_baseline, write_baseline};
use crate::cache::CacheOptions;
use crate::config::{ProjectMode, RuntimeOverrides};
use crate::entry::{
    EntryPlan, EntryWarning, MemberEntryInput, build_entry_roots, mark_command_scripts_dev,
};
use crate::fix::{FixOptions, FixReport, WorkspaceFixManifest, apply_fixes_with_workspace};
use crate::graph::{GraphError, ProjectGraph, add_parsed_imports, build_graph_skeleton};
use crate::manifest::{DeclaredDependency, normalize_distribution_name};
use crate::parser::{
    ParseSeverity, ParseSummary, ParsedModule, apply_module_loaders,
    parse_project_sources_with_cache,
};
use crate::plugins::{PluginExtractRequest, extract_plugin_hints_with_parse};
use crate::reachability::{
    ReachabilityReport, analyze_reachability, apply_distributed_surfaces, apply_member_surfaces,
    apply_public_surface,
};
use crate::reporters::{Diagnostic, FileCounts};
use crate::resolver::{
    ResolveWarning, ScopedDeclarations, StdlibRange, apply_resolution_to_graph, import_root,
    resolve_imports_for_analysis,
};
use crate::rules::deps::local_source_member;
use crate::rules::emit::compute_exit_status;
use crate::rules::{
    DependencyRuleContext, Gap, GapKind, Issue, IssueReport, MemberManifest, RuleContext,
    WorkspaceDependencyBoundary, collect_gaps, emit_issues,
};
use crate::sources::{
    DiscoveredFile, DiscoveredSources, FileContext, PublicSurface, SourcesWarning,
};

use super::error::AnalyzeError;
use super::probe::{ProbeReport, WorkspaceMemberInputs, probe_project_with_cache};
use super::warnings::{ProbeWarning, SkipReason, diagnostic_lines};

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
    /// Every probe and analysis warning's text, once each.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        diagnostic_lines(self.probe.warnings.iter().chain(&self.warnings))
    }

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
#[expect(
    clippy::needless_pass_by_value,
    reason = "callers build the options for this one call; borrowing would only add `&` at each call site"
)]
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
        let fix = apply_fixes_with_workspace(
            &core.issues,
            &probe.root,
            &probe.manifest,
            &workspace_manifests,
            options.fix,
        );
        if !options.update_baseline {
            core.issues.exit_status = exit_status_after_fix(&core.issues, &fix, overrides);
        }
        Some(fix)
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
        let written = write_baseline(issues, root, path)?;
        // Recording the current findings is the point of the run, so it is
        // not a failure; the next `--baseline` run judges them (spec §2).
        issues.exit_status = ExitStatus::Success;
        return Ok(Some(written));
    }
    Ok(Some(apply_baseline(issues, root, path, overrides)?))
}

/// Exit status over the findings `--fix` left standing: an applied edit no
/// longer counts, while `--dry-run` previews and skipped fixes still do (spec §13).
fn exit_status_after_fix(
    issues: &IssueReport,
    fix: &FixReport,
    overrides: &RuntimeOverrides,
) -> ExitStatus {
    let resolved: BTreeSet<usize> = fix.resolved_issues.iter().copied().collect();
    let remaining: Vec<Issue> = issues
        .issues
        .iter()
        .enumerate()
        .filter(|(index, _)| !resolved.contains(index))
        .map(|(_, issue)| issue.clone())
        .collect();
    compute_exit_status(&remaining, overrides, overrides.strict.unwrap_or(false))
}

struct AnalysisCore {
    graph: ProjectGraph,
    reachability: ReachabilityReport,
    entry: EntryPlan,
    issues: IssueReport,
    warnings: Vec<ProbeWarning>,
}

#[expect(
    clippy::too_many_lines,
    reason = "pipeline steps run in order, each one call; splitting would only pass the results around"
)]
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

    let (parse, member_parse) = parse_with_member_files(probe, &target, &options.cache)?;

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
        dockerfiles: &probe.dockerfiles,
    })?;
    let mut warnings: Vec<ProbeWarning> = parse
        .modules
        .iter()
        .chain(&member_parse)
        .flat_map(parse_warnings)
        .chain(plugins.warnings.iter().cloned().map(ProbeWarning::Plugin))
        .collect();
    let mut sources = probe.sources.clone();
    mark_command_scripts_dev(
        &mut sources,
        &probe.effective_config,
        &probe.manifest,
        &plugins,
        &parse,
    );
    let sources = &sources;

    let members: Vec<MemberEntryInput<'_>> = probe
        .workspace_inputs
        .iter()
        .map(|input| (input.member.path.as_str(), &input.manifest, &input.sources))
        .collect();
    let mut entry = build_entry_roots(
        &probe.effective_config,
        &probe.manifest,
        sources,
        &members,
        &plugins,
        production,
    );
    crate::entry::add_script_roots(&mut entry, &probe.scripts, sources, production);
    crate::entry::add_build_backend_roots(&mut entry, &parse, &sources.files);
    crate::entry::add_main_block_roots(&mut entry, &parse, &sources.files, production);
    let entry_warnings = entry_warnings(&entry, &probe.sources);
    let skipped_entries: Vec<String> = entry_warnings
        .iter()
        .filter_map(|warning| match warning {
            ProbeWarning::Entry(EntryWarning::MissingEntryPath { path }) => Some(path.clone()),
            _ => None,
        })
        .collect();
    warnings.extend(entry_warnings);

    let mut graph = build_analysis_graph(probe, sources, &parse, &plugins)?;

    let plugin_refs: Vec<_> = plugins.module_refs().cloned().collect();
    let sys_path_imports: Vec<_> = plugins.sys_path_imports().cloned().collect();
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
        sources,
        &parse,
        &plugin_refs,
        &sys_path_imports,
        &probe.local_trees,
        &script_targets,
        &scoped_declarations(probe),
    );
    apply_resolution_to_graph(&mut graph, &resolution)?;
    warnings.extend(
        resolution
            .warnings
            .iter()
            .filter(|warning| !matches!(warning, ResolveWarning::UnresolvedImport { .. }))
            .cloned()
            .map(ProbeWarning::Resolve),
    );

    let mut reachability =
        analyze_reachability(&mut graph, sources, &entry, &plugins, &parse, production)?;
    let surface = PublicSurface::resolve(
        probe.manifest.metadata.wheel_targets.as_ref(),
        &sources.files,
    );
    if let Some(surface) = &surface {
        apply_public_surface(&mut reachability, surface, &entry);
    }
    // CHK006/CHK007 need every member's surface, app members too (#678). A
    // named member without wheel targets ships its layout's packages (#733);
    // only declared targets narrow reachability.
    let member_surfaces: Vec<_> = probe
        .workspace_inputs
        .iter()
        .map(|input| (input.member.path.clone(), shipped_surface(input)))
        .collect();
    let library_surfaces: Vec<_> = probe
        .workspace_inputs
        .iter()
        .zip(&member_surfaces)
        .filter(|(input, (member, surface))| {
            input.manifest.metadata.wheel_targets.is_some()
                && !surface.files.is_empty()
                && entry.library_members.contains(member)
        })
        .map(|(_, pair)| pair)
        .collect();
    apply_member_surfaces(&mut reachability, &library_surfaces);
    // A member a metapackage root distributes stays an app; only the orphans
    // its wheel ships are library orphans (#1060). Widening needs no declared
    // target: a wrong layout guess only lowers confidence.
    let distributed_surfaces: Vec<_> = member_surfaces
        .iter()
        .filter_map(|pair| {
            let member = entry
                .distributed_members
                .iter()
                .find(|distributed| distributed.member == pair.0)?;
            Some((pair, member.reason.as_str()))
        })
        .collect();
    apply_distributed_surfaces(&mut reachability, &distributed_surfaces, &entry);

    let stdlib = StdlibRange::new(&target, probe.manifest.metadata.requires_python.as_deref());
    let member_imports: Vec<Vec<String>> = probe
        .workspace_inputs
        .iter()
        .map(|input| uninventoried_imports(&input.member.path, &member_parse, stdlib))
        .collect();
    let workspace_boundaries = probe
        .workspace_inputs
        .iter()
        .zip(&member_imports)
        .map(|(input, imports)| WorkspaceDependencyBoundary {
            member_id: &input.member.id,
            manifest: &input.manifest,
            files: &input.sources.files,
            imports,
        })
        .collect::<Vec<_>>();

    let context = RuleContext {
        resolution: &resolution,
        reachability: &reachability,
        graph: &graph,
        sources,
        parse: &parse,
        surface: surface.as_ref(),
    };
    let production_tests = if production
        && (entry.mode == ProjectMode::Library || !entry.library_members.is_empty())
    {
        Some(parse_production_tests(probe, &target)?)
    } else {
        None
    };
    // A test the parser could not read is evidence lost, like a root file.
    warnings.extend(production_tests.iter().flatten().flat_map(parse_warnings));
    // Both rule sets only read the analysis; run them side by side.
    let (deps, symbols) = crate::parallel::join(
        parse.modules.len(),
        || {
            crate::rules::deps::reconcile_with_context(
                &DependencyRuleContext {
                    rules: &context,
                    config: &probe.effective_config,
                    strict,
                    build_scripts: &probe.build_scripts,
                    local: &probe.local_trees,
                },
                &probe.manifest,
                &plugins,
                &workspace_boundaries,
                &probe.scripts,
            )
        },
        || {
            crate::rules::symbols::analyze_with_context(
                &context,
                &entry,
                &plugins,
                &probe.manifest,
                &member_surfaces,
                production_tests.as_deref(),
            )
        },
    );

    let members: Vec<MemberManifest<'_>> = probe
        .workspace_inputs
        .iter()
        .map(|input| MemberManifest {
            id: &input.member.id,
            path: &input.member.path,
            manifest: &input.manifest,
        })
        .collect();
    let gaps = collect_gaps(
        &context,
        &probe.manifest,
        &members,
        &probe.scripts,
        &plugins,
        &skipped_entries,
        &probe.unread_path_sources,
        &probe.effective_config.dependencies,
        stdlib,
    );
    warnings.extend(opaque_import_warning(&gaps));
    warnings.extend(archive_requirement_warning(&gaps));
    warnings.extend(command_target_warnings(&gaps));
    warnings.extend(string_module_ref_warning(&gaps));
    let issues = emit_issues(
        &reachability,
        &deps,
        &symbols,
        &parse,
        &gaps,
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

/// The entry plan's warnings, less the config entries that source discovery
/// already reported missing or a directory. A target two extractors read
/// (a uvicorn `project.scripts` value) is reported once.
fn entry_warnings(entry: &EntryPlan, sources: &DiscoveredSources) -> Vec<ProbeWarning> {
    let reported: HashSet<&str> = sources
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            SourcesWarning::MissingEntryPath { path }
            | SourcesWarning::EntryPathIsDirectory { path } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    let mut warnings: Vec<ProbeWarning> = Vec::new();
    for warning in &entry.warnings {
        if matches!(warning, EntryWarning::MissingEntryPath { path } if reported.contains(path.as_str()))
        {
            continue;
        }
        let warning = ProbeWarning::Entry(warning.clone());
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }
    warnings
}

/// The other gaps reach the user through the parse or manifest warning that
/// recorded them; an opaque import has none (ADR 0005 §3, #909).
fn opaque_import_warning(gaps: &[Gap]) -> Option<ProbeWarning> {
    let mut files = gaps
        .iter()
        .filter(|gap| gap.kind == GapKind::OpaqueDynamicImport)
        .map(|gap| gap.file.clone());
    let path = files.next()?;
    Some(ProbeWarning::OpaqueDynamicImport {
        path,
        more: files.count(),
    })
}

/// An archive line has no manifest warning either: pip accepts it (#988).
/// The warning names the first line by file and line.
fn archive_requirement_warning(gaps: &[Gap]) -> Option<ProbeWarning> {
    let mut sites: Vec<(&str, Option<u32>, &str)> = gaps
        .iter()
        .filter(|gap| {
            matches!(
                gap.kind,
                GapKind::ArchiveRequirement | GapKind::ArchiveRuntimeRequirement
            )
        })
        .filter_map(|gap| {
            let (name, line) = gap.site.as_ref()?;
            Some((gap.file.as_str(), *line, name.as_str()))
        })
        .collect();
    sites.sort_by_key(|(file, line, _)| (*file, line.is_none(), *line));
    let (path, line, name) = *sites.first()?;
    Some(ProbeWarning::ArchiveRequirement {
        path: path.to_owned(),
        line,
        name: name.to_owned(),
        more: sites.len() - 1,
    })
}

/// One warning per command file, at its first target (#913).
fn command_target_warnings(gaps: &[Gap]) -> Vec<ProbeWarning> {
    let mut by_file: BTreeMap<&str, Vec<&(String, Option<u32>)>> = BTreeMap::new();
    for gap in gaps {
        if let (GapKind::UnreadCommandTarget, Some(site)) = (gap.kind, &gap.site) {
            by_file.entry(&gap.file).or_default().push(site);
        }
    }
    by_file
        .into_iter()
        .filter_map(|(path, mut sites)| {
            sites.sort_by_key(|(_, line)| (line.is_none(), *line));
            let (target, line) = sites.first()?;
            Some(ProbeWarning::UnreadCommandTarget {
                path: path.to_owned(),
                line: *line,
                target: target.clone(),
                more: sites.len() - 1,
            })
        })
        .collect()
}

/// A string naming a module has no warning of its own either: one per run,
/// at the first literal by file and line (#1026).
fn string_module_ref_warning(gaps: &[Gap]) -> Option<ProbeWarning> {
    let mut sites: Vec<(&str, u32, &str, &str)> = gaps
        .iter()
        .filter(|gap| gap.kind == GapKind::StringModuleRef)
        .filter_map(|gap| {
            let (text, line) = gap.site.as_ref()?;
            Some((
                gap.file.as_str(),
                (*line)?,
                text.as_str(),
                gap.target.as_deref()?,
            ))
        })
        .collect();
    sites.sort_unstable();
    let (path, line, text, target) = *sites.first()?;
    Some(ProbeWarning::StringModuleRef {
        path: path.to_owned(),
        line,
        text: text.to_owned(),
        target: target.to_owned(),
        more: sites.len() - 1,
    })
}

/// What the user needs to know about one parsed module: a skipped module is
/// one warning naming why, a kept one is a warning per diagnostic (#877).
fn parse_warnings(module: &ParsedModule) -> Vec<ProbeWarning> {
    if module.skipped {
        return vec![ProbeWarning::SkippedSource {
            path: module.path.clone(),
            reason: skip_reason(module),
        }];
    }
    module
        .diagnostics
        .iter()
        .map(|diagnostic| ProbeWarning::ParseDiagnostic {
            path: module.path.clone(),
            line: diagnostic.line,
            message: diagnostic.message.clone(),
        })
        .collect()
}

/// Why a skipped module was left out: a module the parser stopped on carries
/// its syntax error as an error diagnostic, a notebook without readable cells
/// carries a warning naming why, and an undecodable one carries none.
fn skip_reason(module: &ParsedModule) -> SkipReason {
    if let Some(diagnostic) = module
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity == ParseSeverity::Error)
    {
        return SkipReason::SyntaxError {
            line: diagnostic.line,
            message: diagnostic.message.clone(),
        };
    }
    module
        .diagnostics
        .first()
        .map_or(SkipReason::Undecodable, |diagnostic| {
            SkipReason::InvalidNotebook {
                message: diagnostic.message.clone(),
            }
        })
}

/// Parse the root inventory together with the files of the root's
/// `workspace` / `path` source members that it misses, and split the two.
///
/// A root that scans only its own package (`src/**`) never sees its members'
/// files, yet their imports decide which other members are used (#712). Only
/// that check reads the second list, so every other rule still sees the root
/// inventory alone. One parse call keeps both in the same parse cache bundle.
fn parse_with_member_files(
    probe: &ProbeReport,
    target: &crate::config::TargetVersion,
    cache: &CacheOptions,
) -> Result<(ParseSummary, Vec<ParsedModule>), AnalyzeError> {
    let member_files = uninventoried_member_files(probe);
    let member_paths: HashSet<String> = member_files.iter().map(|file| file.path.clone()).collect();
    let mut sources = probe.sources.clone();
    sources.files.extend(member_files);
    let mut parse = parse_project_sources_with_cache(&probe.root, &sources, target, Some(cache))?;
    apply_module_loaders(&mut parse.modules, &probe.effective_config.module_loaders);
    let (members, modules) = parse
        .modules
        .into_iter()
        .partition(|module| member_paths.contains(&module.path));
    Ok((ParseSummary { modules }, members))
}

/// Root-relative files of the root's source members that the root inventory
/// lacks.
fn uninventoried_member_files(probe: &ProbeReport) -> Vec<DiscoveredFile> {
    let mut seen: HashSet<String> = probe
        .sources
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    let mut files = Vec::new();
    for input in &probe.workspace_inputs {
        if local_source_member(&probe.manifest, &probe.local_trees, &input.manifest).is_none() {
            continue;
        }
        for file in &input.sources.files {
            let path = format!("{}/{}", input.member.path, file.path);
            if seen.insert(path.clone()) {
                files.push(DiscoveredFile {
                    path,
                    ..file.clone()
                });
            }
        }
    }
    files
}

/// Non-stdlib modules the parsed files under `member_path` import, named as
/// the resolver names them (`from a import b` is `a.b`). The parser already
/// resolved relative imports through the member's layout; one it could not
/// resolve has an empty module and is dropped.
fn uninventoried_imports(
    member_path: &str,
    modules: &[ParsedModule],
    stdlib: StdlibRange,
) -> Vec<String> {
    let prefix = format!("{member_path}/");
    let mut imports = Vec::new();
    for module in modules
        .iter()
        .filter(|module| module.path.starts_with(&prefix))
    {
        let statics = module
            .imports
            .iter()
            .filter(|import| !import.module.is_empty())
            .map(|import| match &import.name {
                Some(name) => format!("{}.{name}", import.module),
                None => import.module.clone(),
            });
        let named = module
            .dynamic_imports
            .iter()
            .chain(&module.pytest_plugins)
            .map(|import| import.module.clone());
        imports.extend(
            statics
                .chain(named)
                .filter(|name| !stdlib.contains(import_root(name))),
        );
    }
    imports
}

/// Member-relative files the member's wheel ships; empty when it ships none,
/// as a member without `[build-system]` that uv does not package.
fn shipped_surface(input: &WorkspaceMemberInputs) -> PublicSurface {
    let metadata = &input.manifest.metadata;
    let files = &input.sources.files;
    match &metadata.wheel_targets {
        Some(targets) => PublicSurface::resolve(Some(targets), files),
        None if metadata.name.is_some() && metadata.build_backend.is_some() => {
            PublicSurface::from_layout(&input.sources.layout, files)
        },
        None => None,
    }
    .unwrap_or_default()
}

/// `--production` drops tests from discovery, but a library's tests are still
/// the evidence that a public module's name is called from outside (#588).
/// The root walk kept them aside, and what it could not read is already among
/// the probe's warnings (#899, #844).
fn parse_production_tests(
    probe: &ProbeReport,
    target: &crate::config::TargetVersion,
) -> Result<Vec<ParsedModule>, AnalyzeError> {
    let tests = DiscoveredSources {
        files: probe.production_tests.clone(),
        ..probe.sources.clone()
    };
    let mut modules = parse_project_sources_with_cache(&probe.root, &tests, target, None)?.modules;
    apply_module_loaders(&mut modules, &probe.effective_config.module_loaders);
    Ok(modules)
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
            .chain(probe.build_scripts.clone())
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
    sources: &DiscoveredSources,
    parse: &crate::parser::ParseSummary,
    plugins: &crate::plugins::PluginHints,
) -> Result<ProjectGraph, AnalyzeError> {
    let mut graph = build_graph_skeleton(&probe.manifest, sources)?;
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
    use crate::parser::ParseDiagnostic;
    use crate::rules::IssueSubject;
    use crate::rules::RuleId;
    use crate::rules::Severity;

    /// A syntax error names the skip even after an earlier warning; a
    /// warning alone is the notebook's reason, and no diagnostic means the
    /// bytes did not decode (#893).
    #[test]
    fn skip_reason_prefers_the_syntax_error() {
        let diagnostic = |severity, message: &str| ParseDiagnostic {
            line: 4,
            message: message.to_owned(),
            severity,
        };
        let mut module = ParsedModule::default();

        assert_eq!(skip_reason(&module), SkipReason::Undecodable);
        module
            .diagnostics
            .push(diagnostic(ParseSeverity::Warning, "invalid notebook JSON"));
        assert_eq!(
            skip_reason(&module),
            SkipReason::InvalidNotebook {
                message: "invalid notebook JSON".to_owned()
            }
        );
        module
            .diagnostics
            .push(diagnostic(ParseSeverity::Error, "Expected a statement"));
        assert_eq!(
            skip_reason(&module),
            SkipReason::SyntaxError {
                line: 4,
                message: "Expected a statement".to_owned()
            }
        );
    }

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
