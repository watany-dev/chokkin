//! Reachability analysis orchestration.

use indexmap::IndexSet;

use crate::config::{Confidence, ProjectMode};
use crate::entry::EntryPlan;
use crate::graph::ProjectGraph;
use crate::parser::ParseSummary;
use crate::plugins::PluginHints;
use crate::sources::{
    DiscoveredSources, FileContext, FileKind, PublicSurface, build_glob_set, member_ships,
};

use super::bfs::run_reachability_bfs;
use super::error::ReachabilityError;
use super::module_index::ModuleIndex;
use super::types::{ReachPredecessor, ReachabilityReport, TraceStep, UnreachableFile};

/// Analyze file reachability from entry roots (pipeline step 9).
///
/// # Errors
///
/// Returns `ReachabilityError` when framework globs cannot be compiled.
#[allow(clippy::too_many_arguments)]
pub fn analyze_reachability(
    graph: &mut ProjectGraph,
    sources: &DiscoveredSources,
    entry: &EntryPlan,
    plugins: &PluginHints,
    parse: &ParseSummary,
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
    // A reachable skipped file's imports are as unknown as an opaque one's.
    let reached_opaque_dynamic_import = parse.modules.iter().any(|module| {
        (module.has_opaque_dynamic_import || module.skipped)
            && graph
                .file_id(&module.path)
                .is_some_and(|file_id| reachable.contains(&file_id))
    });

    // Nothing was read from a skipped file, so it is neither known unused nor
    // a reliable source of edges; reporting it would only be a guess.
    let skipped: std::collections::HashSet<&str> = parse
        .modules
        .iter()
        .filter(|module| module.skipped)
        .map(|module| module.path.as_str())
        .collect();
    let mut unreachable = Vec::new();
    for file in &sources.files {
        if !matches!(file.kind, FileKind::Python | FileKind::Notebook) {
            continue;
        }
        if production && !file.context.is_included_in_production() {
            continue;
        }
        let mode = entry.mode_for(&file.path);
        if is_excluded(file, mode) || skipped.contains(file.path.as_str()) {
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
            max_confidence: confidence_for_unreachable(mode, reached_opaque_dynamic_import),
            mode,
        });
    }

    unreachable.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(ReachabilityReport {
        reachable,
        eager: bfs.eager,
        certain: bfs.certain,
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
/// scored as in app mode. The surface is the root distribution's, so files of
/// library workspace members, which ship in their own wheels, keep `Maybe`.
pub fn apply_public_surface(
    report: &mut ReachabilityReport,
    surface: &PublicSurface,
    entry: &EntryPlan,
) {
    if entry.mode != ProjectMode::Library {
        return;
    }
    let confidence =
        confidence_for_unreachable(ProjectMode::App, report.reached_opaque_dynamic_import);
    for file in &mut report.unreachable {
        if !surface.contains(&file.path) && !entry.in_library_member(&file.path) {
            file.max_confidence = confidence;
        }
    }
}

/// Re-score library-member orphans that the member's own wheel does not ship
/// (R-05, #587).
///
/// `surfaces` pairs each library member's root-relative directory with its
/// surface, whose paths are relative to that member.
pub(crate) fn apply_member_surfaces(
    report: &mut ReachabilityReport,
    surfaces: &[&(String, PublicSurface)],
) {
    let confidence =
        confidence_for_unreachable(ProjectMode::App, report.reached_opaque_dynamic_import);
    for file in &mut report.unreachable {
        if member_ships(surfaces.iter().copied(), &file.path) == Some(false) {
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

    let set = build_glob_set(&patterns).map_err(
        |crate::sources::SourcesError::InvalidGlob { pattern, reason }| {
            ReachabilityError::InvalidFrameworkGlob { pattern, reason }
        },
    )?;

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

fn is_excluded(file: &crate::sources::DiscoveredFile, mode: ProjectMode) -> bool {
    file.path.ends_with("__init__.py")
        || (file.context == FileContext::Test && mode == ProjectMode::Library)
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
            config_declared_distributions: Vec::new(),
            config_provided_binaries: Vec::new(),
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
                    deferred: false,
                    relative_level: 0,
                })
                .collect(),
            has_opaque_dynamic_import: opaque,
            ..ParsedModule::default()
        }
    }

    fn app_entry(roots: &[&str]) -> EntryPlan {
        EntryPlan {
            mode: ProjectMode::App,
            library_members: Vec::new(),
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
                ..Default::default()
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
        let report = analyze_reachability(&mut graph, &sources, &entry, plugins, parse, false)
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
        assert_eq!(report.unreachable, []);
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

    #[test]
    fn library_member_orphans_are_maybe_while_app_orphans_stay_certain() {
        let parse = ParseSummary {
            modules: ["acme/main.py", "acme/orphan.py", "libs/core/pkg/orphan.py"]
                .map(|path| parsed(path, &[], false))
                .into(),
        };
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        };
        let mut paths: Vec<&str> = parse.modules.iter().map(|m| m.path.as_str()).collect();
        paths.push("libs/core/tests/test_x.py");
        let mut sources = flat_sources(root, &paths);
        if let Some(test) = sources.files.last_mut() {
            test.context = FileContext::Test;
        }
        let mut graph = graph_for(&sources, &parse, &[]);
        let mut entry = app_entry(&["acme/main.py"]);
        entry.library_members = vec!["libs/core".to_owned()];
        let report =
            analyze_reachability(&mut graph, &sources, &entry, &no_plugins(), &parse, false)
                .expect("reachability");

        let found: Vec<_> = report
            .unreachable
            .iter()
            .map(|file| (file.path.as_str(), file.max_confidence, file.mode))
            .collect();
        assert_eq!(
            found,
            [
                ("acme/orphan.py", Confidence::Certain, ProjectMode::App),
                (
                    "libs/core/pkg/orphan.py",
                    Confidence::Maybe,
                    ProjectMode::Library
                ),
            ]
        );
    }

    #[test]
    fn library_root_lifts_its_orphans_but_not_library_members() {
        let parse = ParseSummary {
            modules: [
                "acme/__init__.py",
                "acme/orphan.py",
                "libs/core/pkg/orphan.py",
            ]
            .map(|path| parsed(path, &[], false))
            .into(),
        };
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        };
        let paths: Vec<&str> = parse.modules.iter().map(|m| m.path.as_str()).collect();
        let sources = flat_sources(root, &paths);
        let mut graph = graph_for(&sources, &parse, &[]);
        let mut entry = app_entry(&["acme/__init__.py"]);
        entry.mode = ProjectMode::Library;
        entry.library_members = vec!["libs/core".to_owned()];
        let mut report =
            analyze_reachability(&mut graph, &sources, &entry, &no_plugins(), &parse, false)
                .expect("reachability");
        let surface = PublicSurface {
            files: std::collections::BTreeSet::new(),
        };
        apply_public_surface(&mut report, &surface, &entry);

        let found: Vec<_> = report
            .unreachable
            .iter()
            .map(|file| (file.path.as_str(), file.max_confidence))
            .collect();
        assert_eq!(
            found,
            [
                ("acme/orphan.py", Confidence::Certain),
                ("libs/core/pkg/orphan.py", Confidence::Maybe),
            ]
        );
    }

    #[test]
    fn library_member_lifts_orphans_its_wheel_does_not_ship() {
        let parse = ParseSummary {
            modules: [
                "acme/main.py",
                "libs/core/pkg/orphan.py",
                "libs/core/docs/conf.py",
                "libs/core/plugins/x/pkg/orphan.py",
            ]
            .map(|path| parsed(path, &[], false))
            .into(),
        };
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        };
        let paths: Vec<&str> = parse.modules.iter().map(|m| m.path.as_str()).collect();
        let sources = flat_sources(root, &paths);
        let mut graph = graph_for(&sources, &parse, &[]);
        let mut entry = app_entry(&["acme/main.py"]);
        entry.library_members = vec!["libs/core".to_owned(), "libs/core/plugins/x".to_owned()];
        let mut report =
            analyze_reachability(&mut graph, &sources, &entry, &no_plugins(), &parse, false)
                .expect("reachability");
        let surface = |path: &str| PublicSurface {
            files: [path.to_owned()].into(),
        };
        apply_member_surfaces(
            &mut report,
            &[
                &("libs/core".to_owned(), surface("pkg/orphan.py")),
                &("libs/core/plugins/x".to_owned(), surface("pkg/orphan.py")),
            ],
        );

        let found: Vec<_> = report
            .unreachable
            .iter()
            .map(|file| (file.path.as_str(), file.max_confidence))
            .collect();
        assert_eq!(
            found,
            [
                ("libs/core/docs/conf.py", Confidence::Certain),
                ("libs/core/pkg/orphan.py", Confidence::Maybe),
                ("libs/core/plugins/x/pkg/orphan.py", Confidence::Maybe),
            ]
        );
    }

    #[test]
    fn skipped_reachable_file_lowers_orphans_and_is_not_reported() {
        let parse = ParseSummary {
            modules: vec![
                parsed("acme/main.py", &["acme.latin"], false),
                ParsedModule {
                    path: "acme/latin.py".to_owned(),
                    skipped: true,
                    ..ParsedModule::default()
                },
                parsed("acme/orphan.py", &[], false),
                ParsedModule {
                    path: "acme/stray.py".to_owned(),
                    skipped: true,
                    ..ParsedModule::default()
                },
            ],
        };
        let origins = [("acme.latin", ModuleOrigin::FirstParty)];
        let (_, report) = analyze(&parse, &[], &origins, &["acme/main.py"], &no_plugins());
        let unreachable: Vec<_> = report
            .unreachable
            .iter()
            .map(|file| (file.path.as_str(), file.max_confidence))
            .collect();
        assert_eq!(unreachable, [("acme/orphan.py", Confidence::Likely)]);
    }
}
