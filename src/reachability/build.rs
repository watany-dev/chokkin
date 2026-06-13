//! Reachability analysis orchestration.

use indexmap::IndexSet;

use globset::{Glob, GlobSetBuilder};

use crate::config::{Confidence, ProjectMode};
use crate::entry::{EntryPlan, ResolvedMode};
use crate::graph::ProjectGraph;
use crate::parser::ParseSummary;
use crate::plugins::PluginHints;
use crate::sources::{DiscoveredSources, FileContext, FileKind};

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
    let module_index = ModuleIndex::build(graph, sources);
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
        if file.kind != FileKind::Python {
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
        if reasons.iter().any(|reason| {
            matches!(
                reason,
                UnreachableReason::ExcludedInit
                    | UnreachableReason::ExcludedStub
                    | UnreachableReason::ExcludedTestContext
                    | UnreachableReason::ExcludedProductionContext
            )
        }) {
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

    let mut builder = GlobSetBuilder::new();
    for pattern in &patterns {
        let glob = Glob::new(pattern).map_err(|error| ReachabilityError::InvalidFrameworkGlob {
            pattern: pattern.clone(),
            reason: error.to_string(),
        })?;
        builder.add(glob);
    }
    let set = builder
        .build()
        .map_err(|error| ReachabilityError::InvalidFrameworkGlob {
            pattern: patterns.join(", "),
            reason: error.to_string(),
        })?;

    let mut framework_used = IndexSet::new();
    for file in &sources.files {
        if file.kind != FileKind::Python {
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
    if file.kind == FileKind::Stub {
        reasons.push(UnreachableReason::ExcludedStub);
    }
    if file.context == FileContext::Test && mode.mode == ProjectMode::Library {
        reasons.push(UnreachableReason::ExcludedTestContext);
    }
    if production && !file.context.is_included_in_production() {
        reasons.push(UnreachableReason::ExcludedProductionContext);
    }

    reasons
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
