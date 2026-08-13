//! Reachability analysis orchestration.

use indexmap::IndexSet;

use crate::cache::CacheOptions;
use crate::config::{Confidence, ProjectMode};
use crate::entry::{EntryPlan, ResolvedMode};
use crate::graph::ProjectGraph;
use crate::parser::ParseSummary;
use crate::plugins::PluginHints;
use crate::sources::{DiscoveredSources, FileContext, FileKind, build_glob_set};

use super::bfs::run_reachability_bfs;
use super::error::ReachabilityError;
use super::module_index::ModuleIndex;
use super::types::{ReachabilityReport, UnreachableFile, UnreachableReason};

/// Analyze file reachability from entry roots (pipeline step 9).
///
/// # Errors
///
/// Returns [`ReachabilityError`] when framework globs cannot be compiled.
#[allow(clippy::too_many_arguments)]
pub fn analyze_reachability(
    graph: &mut ProjectGraph,
    sources: &DiscoveredSources,
    entry: &EntryPlan,
    plugins: &PluginHints,
    parse: &ParseSummary,
    mode: &ResolvedMode,
    production: bool,
) -> Result<ReachabilityReport, ReachabilityError> {
    analyze_reachability_with_cache(
        graph, sources, entry, plugins, parse, mode, production, None,
    )
}

/// Analyze file reachability with optional cached module index support.
///
/// # Errors
///
/// Returns [`ReachabilityError`] when framework globs cannot be compiled or cache I/O fails.
#[allow(clippy::too_many_arguments)]
pub fn analyze_reachability_with_cache(
    graph: &mut ProjectGraph,
    sources: &DiscoveredSources,
    entry: &EntryPlan,
    plugins: &PluginHints,
    parse: &ParseSummary,
    mode: &ResolvedMode,
    production: bool,
    cache: Option<&CacheOptions>,
) -> Result<ReachabilityReport, ReachabilityError> {
    let module_index = ModuleIndex::build_with_cache(graph, sources, cache).map_err(|source| {
        ReachabilityError::Invariant {
            detail: format!("module index cache I/O failed: {source}"),
        }
    })?;
    let bfs = run_reachability_bfs(graph, entry, plugins, parse, &module_index);

    let framework_used = apply_framework_globs(graph, sources, plugins)?;
    let mut reachable: IndexSet<_> = bfs.reachable.into_iter().collect();
    for file_id in &framework_used {
        reachable.insert(*file_id);
    }

    let parse_by_path = parse
        .modules
        .iter()
        .map(|module| (module.path.as_str(), module))
        .collect::<std::collections::HashMap<_, _>>();

    let mut unreachable = Vec::new();
    for file in &sources.files {
        if !matches!(file.kind, FileKind::Python | FileKind::Notebook) {
            continue;
        }
        if production && !file.context.is_included_in_production() {
            continue;
        }
        let Some(file_id) = graph.file_id(&file.path) else {
            continue;
        };
        if reachable.contains(&file_id) {
            continue;
        }

        let reasons = exclusion_reasons(file, mode, production);
        if is_hard_excluded(&reasons) {
            continue;
        }

        let parsed = parse_by_path.get(file.path.as_str()).copied();
        unreachable.push(UnreachableFile {
            file: file_id,
            path: file.path.clone(),
            reasons,
            max_confidence: confidence_for_unreachable(mode.mode, parsed),
        });
    }

    unreachable.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(ReachabilityReport {
        reachable,
        unreachable,
        used_modules: bfs.used_modules,
        framework_used,
        predecessors: bfs.predecessors,
    })
}

fn apply_framework_globs(
    graph: &ProjectGraph,
    sources: &DiscoveredSources,
    plugins: &PluginHints,
) -> Result<IndexSet<crate::graph::FileId>, ReachabilityError> {
    let patterns: Vec<String> = plugins
        .framework_used_globs()
        .map(|glob| glob.pattern.clone())
        .collect();
    if patterns.is_empty() {
        return Ok(IndexSet::new());
    }

    let set = build_glob_set(&patterns).map_err(|error| match error {
        crate::sources::SourcesError::InvalidGlob { pattern, reason } => {
            ReachabilityError::InvalidFrameworkGlob { pattern, reason }
        },
        crate::sources::SourcesError::Io { .. } => ReachabilityError::Invariant {
            detail: "unexpected I/O error while compiling framework globs".to_owned(),
        },
    })?;

    let mut framework_used = IndexSet::new();
    for file in &sources.files {
        if !matches!(file.kind, FileKind::Python | FileKind::Notebook) {
            continue;
        }
        if !set.is_match(&file.path) {
            continue;
        }
        if let Some(file_id) = graph.file_id(&file.path) {
            framework_used.insert(file_id);
        }
    }
    Ok(framework_used)
}

fn exclusion_reasons(
    file: &crate::sources::DiscoveredFile,
    mode: &ResolvedMode,
    production: bool,
) -> Vec<UnreachableReason> {
    let mut reasons = vec![UnreachableReason::NotReachable];

    if file.path.ends_with("__init__.py") {
        reasons.push(UnreachableReason::ExcludedInit);
    }
    if file.context == FileContext::Test && mode.mode == ProjectMode::Library {
        reasons.push(UnreachableReason::ExcludedTestContext);
    }
    if production && !file.context.is_included_in_production() {
        reasons.push(UnreachableReason::ExcludedProductionContext);
    }

    reasons
}

fn is_hard_excluded(reasons: &[UnreachableReason]) -> bool {
    reasons.iter().any(|reason| {
        matches!(
            reason,
            UnreachableReason::ExcludedInit
                | UnreachableReason::ExcludedStub
                | UnreachableReason::ExcludedTestContext
                | UnreachableReason::ExcludedProductionContext
        )
    })
}

fn confidence_for_unreachable(
    mode: ProjectMode,
    parsed: Option<&crate::parser::ParsedModule>,
) -> Confidence {
    if mode == ProjectMode::Library {
        return Confidence::Maybe;
    }
    if parsed.is_some_and(|module| module.has_opaque_dynamic_import) {
        return Confidence::Likely;
    }
    Confidence::Certain
}
