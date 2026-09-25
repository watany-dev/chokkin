//! Reachability analysis orchestration.

use indexmap::IndexSet;

use crate::config::{Confidence, ProjectMode};
use crate::entry::{EntryPlan, ResolvedMode};
use crate::graph::ProjectGraph;
use crate::parser::ParseSummary;
use crate::plugins::PluginHints;
use crate::sources::{DiscoveredSources, FileContext, FileKind, PublicSurface, build_glob_set};

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
    let framework = apply_framework_globs(graph, sources, plugins)?;
    let bfs = run_reachability_bfs(
        graph,
        entry,
        plugins,
        parse,
        &module_index,
        framework.predecessors,
    );
    let reachable = bfs.reachable;
    let reached_opaque_dynamic_import = parse.modules.iter().any(|module| {
        module.has_opaque_dynamic_import
            && graph
                .file_id(&module.path)
                .is_some_and(|file_id| reachable.contains(&file_id))
    });
    let confidence = confidence_for_unreachable(mode.mode, reached_opaque_dynamic_import);

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

        unreachable.push(UnreachableFile {
            file: file_id,
            path: file.path.clone(),
            max_confidence: confidence,
        });
    }

    unreachable.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(ReachabilityReport {
        reachable,
        unreachable,
        used_modules: bfs.used_modules,
        framework_used: framework.files,
        predecessors: bfs.predecessors,
        reached_opaque_dynamic_import,
    })
}

/// Re-score library-mode orphans that the wheel does not ship (R-05).
///
/// Library mode caps orphans at `Maybe` because an outside caller may import
/// them; a file outside the distributed packages has no such caller, so it is
/// scored as in app mode.
///
/// `_parse` is no longer read (the report already knows whether reachable code
/// has an opaque dynamic import); it stays so the public signature is unchanged.
pub fn apply_public_surface(
    report: &mut ReachabilityReport,
    surface: &PublicSurface,
    _parse: &ParseSummary,
    mode: &ResolvedMode,
) {
    if mode.mode != ProjectMode::Library {
        return;
    }
    let confidence =
        confidence_for_unreachable(ProjectMode::App, report.reached_opaque_dynamic_import);
    for file in &mut report.unreachable {
        if !surface.contains(&file.path) {
            file.max_confidence = confidence;
        }
    }
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

/// An opaque `import_module(name)` in code that runs may load any orphan, so
/// it lowers every orphan; one inside an orphan never runs and changes nothing.
const fn confidence_for_unreachable(mode: ProjectMode, reached_opaque: bool) -> Confidence {
    if matches!(mode, ProjectMode::Library) {
        return Confidence::Maybe;
    }
    if reached_opaque {
        return Confidence::Likely;
    }
    Confidence::Certain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PluginId;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::entry::EntryRoot;
    use crate::graph::{FileNode, ModuleOrigin, add_parsed_imports};
    use crate::parser::{ImportContext, ImportKind, ImportRef, ParsedModule};
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
            ..no_plugins()
        }
    }

    fn no_plugins() -> PluginHints {
        PluginHints {
            contributions: Vec::new(),
            config_binary_usages: Vec::new(),
            config_used_distributions: Vec::new(),
            config_module_refs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn parsed(path: &str, imports: &[&str], opaque: bool) -> ParsedModule {
        ParsedModule {
            path: path.to_owned(),
            imports: imports
                .iter()
                .map(|module| ImportRef {
                    module: (*module).to_owned(),
                    name: None,
                    alias: None,
                    line: 1,
                    kind: ImportKind::Import,
                    context: ImportContext::Runtime,
                    optional: false,
                    platform_guarded: false,
                    relative_level: 0,
                })
                .collect(),
            has_opaque_dynamic_import: opaque,
            ..ParsedModule::default()
        }
    }

    fn app_entry(roots: &[&str]) -> EntryPlan {
        EntryPlan {
            mode: ResolvedMode {
                mode: ProjectMode::App,
                confidence: ResolveConfidence::Certain,
            },
            roots: roots
                .iter()
                .map(|path| EntryRoot {
                    spec: crate::config::EntrySpec {
                        path: (*path).to_owned(),
                        symbol: None,
                    },
                    context: FileContext::Runtime,
                    origins: Vec::new(),
                })
                .collect(),
            warnings: Vec::new(),
        }
    }

    fn flat_sources(root: ProjectRoot, paths: &[&str]) -> DiscoveredSources {
        DiscoveredSources {
            root,
            layout: LayoutInfo {
                layout: ProjectLayout::Flat,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: paths
                .iter()
                .map(|path| DiscoveredFile {
                    path: (*path).to_owned(),
                    kind: FileKind::Python,
                    context: FileContext::Runtime,
                })
                .collect(),
            warnings: Vec::new(),
        }
    }

    fn graph_for(
        sources: &DiscoveredSources,
        parse: &ParseSummary,
        origins: &[(&str, ModuleOrigin)],
    ) -> ProjectGraph {
        let mut graph = ProjectGraph::new(sources.root.clone());
        for file in &sources.files {
            graph
                .intern_file(FileNode {
                    path: file.path.clone(),
                    context: file.context,
                    kind: file.kind,
                })
                .expect("file");
        }
        for (module, origin) in origins {
            graph.intern_module((*module).to_owned(), *origin);
        }
        for module in &parse.modules {
            let file_id = graph.file_id(&module.path).expect("parsed file");
            add_parsed_imports(&mut graph, file_id, module).expect("import edges");
        }
        graph
    }

    /// Analyze a flat `acme` project whose files are `parse`'s modules plus `extra`.
    fn analyze(
        parse: &ParseSummary,
        extra: &[&str],
        origins: &[(&str, ModuleOrigin)],
        roots: &[&str],
        plugins: &PluginHints,
    ) -> (ProjectGraph, ReachabilityReport) {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        };
        let paths: Vec<&str> = parse
            .modules
            .iter()
            .map(|module| module.path.as_str())
            .chain(extra.iter().copied())
            .collect();
        let sources = flat_sources(root, &paths);
        let mut graph = graph_for(&sources, parse, origins);
        let entry = app_entry(roots);
        let report = analyze_reachability(
            &mut graph,
            &sources,
            &entry,
            plugins,
            parse,
            &entry.mode,
            false,
        )
        .expect("reachability");
        (graph, report)
    }

    #[test]
    fn framework_glob_file_has_a_non_empty_trace() {
        let (graph, report) = analyze(
            &ParseSummary::default(),
            &[MIGRATION],
            &[],
            &[],
            &plugin_hints(),
        );

        let file_id = graph.file_id(MIGRATION).expect("migration file");
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

    #[test]
    fn framework_glob_file_imports_are_followed() {
        let parse = ParseSummary {
            modules: vec![parsed(MIGRATION, &["acme.models", "django.db"], false)],
        };
        let origins = [
            ("acme.models", ModuleOrigin::FirstParty),
            ("django.db", ModuleOrigin::ThirdParty),
        ];
        let (graph, report) = analyze(&parse, &["acme/models.py"], &origins, &[], &plugin_hints());

        let models = graph.file_id("acme/models.py").expect("models");
        assert!(report.reachable.contains(&models));
        assert!(!report.framework_used.contains(&models));
        assert!(report.unreachable.is_empty());
        assert!(
            report
                .used_modules
                .iter()
                .any(|used| used.import_root == "django" && used.file == MIGRATION)
        );
    }

    fn orphan_confidence(opaque_in: &str) -> Confidence {
        let parse = ParseSummary {
            modules: ["acme/main.py", "acme/orphan.py"]
                .map(|path| parsed(path, &[], path == opaque_in))
                .into(),
        };
        let (_, report) = analyze(&parse, &[], &[], &["acme/main.py"], &no_plugins());
        assert_eq!(report.unreachable.len(), 1);
        report.unreachable[0].max_confidence
    }

    #[test]
    fn opaque_import_in_reachable_code_lowers_orphans_to_likely() {
        assert_eq!(orphan_confidence("acme/main.py"), Confidence::Likely);
    }

    #[test]
    fn opaque_import_inside_the_orphan_itself_keeps_it_certain() {
        assert_eq!(orphan_confidence("acme/orphan.py"), Confidence::Certain);
    }
}
