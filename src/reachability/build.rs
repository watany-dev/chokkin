//! Reachability analysis orchestration.

use indexmap::IndexSet;

use crate::config::{Confidence, ProjectMode};
use crate::entry::{EntryPlan, ResolvedMode};
use crate::graph::ProjectGraph;
use crate::parser::ParseSummary;
use crate::plugins::PluginHints;
use crate::sources::{DiscoveredSources, FileContext, FileKind, build_glob_set};

use super::bfs::run_reachability_bfs;
use super::error::ReachabilityError;
use super::module_index::ModuleIndex;
use super::types::{ReachPredecessor, ReachabilityReport, TraceStep, UnreachableFile};

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
    let bfs = run_reachability_bfs(graph, entry, plugins, &module_index);

    let framework = apply_framework_globs(graph, sources, plugins)?;
    let mut reachable: IndexSet<_> = bfs.reachable.into_iter().collect();
    let mut predecessors = bfs.predecessors;
    for file_id in &framework.files {
        reachable.insert(*file_id);
    }
    // A file only the globs reach has no BFS predecessor, so `--trace` would
    // print an empty path unless the matching glob is recorded as its step.
    for (file_id, predecessor) in framework.predecessors {
        predecessors.entry(file_id).or_insert(predecessor);
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
        if is_excluded(file, mode) {
            continue;
        }
        let Some(file_id) = graph.file_id(&file.path) else {
            continue;
        };
        if reachable.contains(&file_id) {
            continue;
        }

        let parsed = parse_by_path.get(file.path.as_str()).copied();
        unreachable.push(UnreachableFile {
            file: file_id,
            path: file.path.clone(),
            max_confidence: confidence_for_unreachable(mode.mode, parsed),
        });
    }

    unreachable.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(ReachabilityReport {
        reachable,
        unreachable,
        used_modules: bfs.used_modules,
        framework_used: framework.files,
        predecessors,
    })
}

/// Files a plugin's framework glob marks as used, with the glob that matched.
struct FrameworkUsed {
    files: IndexSet<crate::graph::FileId>,
    predecessors: Vec<(crate::graph::FileId, ReachPredecessor)>,
}

fn apply_framework_globs(
    graph: &ProjectGraph,
    sources: &DiscoveredSources,
    plugins: &PluginHints,
) -> Result<FrameworkUsed, ReachabilityError> {
    let labels: Vec<String> = plugins
        .contributions
        .iter()
        .flat_map(|contribution| {
            contribution
                .framework_used_globs
                .iter()
                .map(move |glob| format!("{}:{}", contribution.plugin.as_key(), glob.pattern))
        })
        .collect();
    let patterns: Vec<String> = plugins
        .framework_used_globs()
        .map(|glob| glob.pattern.clone())
        .collect();
    if patterns.is_empty() {
        return Ok(FrameworkUsed {
            files: IndexSet::new(),
            predecessors: Vec::new(),
        });
    }

    let set = build_glob_set(&patterns).map_err(|error| match error {
        crate::sources::SourcesError::InvalidGlob { pattern, reason } => {
            ReachabilityError::InvalidFrameworkGlob { pattern, reason }
        },
        crate::sources::SourcesError::Io { .. } => ReachabilityError::Invariant {
            detail: "unexpected I/O error while compiling framework globs".to_owned(),
        },
    })?;

    let mut files = IndexSet::new();
    let mut predecessors = Vec::new();
    for file in &sources.files {
        if !matches!(file.kind, FileKind::Python | FileKind::Notebook) {
            continue;
        }
        let matched = set.matches(&file.path);
        let Some(first) = matched.first().copied() else {
            continue;
        };
        let Some(file_id) = graph.file_id(&file.path) else {
            continue;
        };
        files.insert(file_id);
        let label = labels
            .get(first)
            .cloned()
            .unwrap_or_else(|| "framework glob".to_owned());
        predecessors.push((
            file_id,
            ReachPredecessor {
                from: None,
                step: TraceStep::PluginRef {
                    module: file.path.clone(),
                    label,
                },
            },
        ));
    }
    Ok(FrameworkUsed {
        files,
        predecessors,
    })
}

fn is_excluded(file: &crate::sources::DiscoveredFile, mode: &ResolvedMode) -> bool {
    file.path.ends_with("__init__.py")
        || (file.context == FileContext::Test && mode.mode == ProjectMode::Library)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PluginId;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::FileNode;
    use crate::plugins::{FrameworkUsedGlob, PluginContribution, ReferenceOrigin};
    use crate::reachability::trace_to_file;
    use crate::resolver::ResolveConfidence;
    use crate::sources::{DiscoveredFile, LayoutInfo, ProjectLayout};

    const MIGRATION: &str = "acme/migrations/0001_initial.py";

    fn plugin_hints() -> PluginHints {
        let mut contribution = PluginContribution::empty(PluginId::Django);
        contribution.framework_used_globs.push(FrameworkUsedGlob {
            pattern: "**/migrations/*.py".to_owned(),
            origin: ReferenceOrigin {
                file: "manage.py".to_owned(),
                line: None,
                label: "django migrations".to_owned(),
            },
        });
        PluginHints {
            contributions: vec![contribution],
            config_binary_usages: Vec::new(),
            config_used_distributions: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn framework_glob_file_has_a_non_empty_trace() {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        };
        let mut graph = ProjectGraph::new(root.clone());
        let file_id = graph
            .intern_file(FileNode {
                path: MIGRATION.to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("migration file");
        let sources = DiscoveredSources {
            root,
            layout: LayoutInfo {
                layout: ProjectLayout::Flat,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: vec![DiscoveredFile {
                path: MIGRATION.to_owned(),
                kind: FileKind::Python,
                context: FileContext::Runtime,
            }],
            warnings: Vec::new(),
        };
        let entry = EntryPlan {
            mode: ResolvedMode {
                mode: ProjectMode::App,
                confidence: ResolveConfidence::Certain,
            },
            roots: Vec::new(),
            warnings: Vec::new(),
        };
        let parse = ParseSummary::default();

        let report = analyze_reachability(
            &mut graph,
            &sources,
            &entry,
            &plugin_hints(),
            &parse,
            &entry.mode,
            false,
        )
        .expect("reachability");

        assert!(report.framework_used.contains(&file_id));
        let trace = trace_to_file(&report, file_id).expect("trace");
        assert_eq!(
            trace.steps,
            vec![TraceStep::PluginRef {
                module: MIGRATION.to_owned(),
                label: "django:**/migrations/*.py".to_owned(),
            }]
        );
    }
}
