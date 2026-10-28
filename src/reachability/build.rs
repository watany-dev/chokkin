//! Reachability analysis orchestration.

use indexmap::IndexSet;

use crate::config::{Confidence, ProjectMode};
use crate::entry::EntryPlan;
use crate::graph::{FileId, ProjectGraph};
use crate::parser::ParseSummary;
use crate::path_util::parent_rel;
use crate::plugins::PluginHints;
use crate::sources::{
    DiscoveredSources, FileContext, FileKind, PublicSurface, build_glob_set, is_test_harness_name,
    member_ships,
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
#[expect(
    clippy::too_many_arguments,
    reason = "takes one output per earlier pipeline phase; a bundle struct would only rename them"
)]
pub fn analyze_reachability(
    graph: &mut ProjectGraph,
    sources: &DiscoveredSources,
    entry: &EntryPlan,
    plugins: &PluginHints,
    parse: &ParseSummary,
    production: bool,
) -> Result<ReachabilityReport, ReachabilityError> {
    let sys_path_imports: Vec<_> = plugins.sys_path_imports().cloned().collect();
    let module_index =
        ModuleIndex::build(graph, sources).with_sys_path_hints(sources, parse, &sys_path_imports);
    let plugin_ref_files = plugins
        .module_refs()
        .filter_map(|reference| {
            let file = module_index.resolve_from(&reference.origin.file, &reference.module)?;
            let key = (reference.origin.file.clone(), reference.module.clone());
            Some((key, graph.file(file)?.path.clone()))
        })
        .collect();
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

    // Nothing was read from a skipped file, so it is neither known unused nor
    // a reliable source of edges; reporting it would only be a guess. It is
    // kept apart, scored all the same, for the gap stage (#550).
    let skipped: std::collections::HashSet<&str> = parse
        .modules
        .iter()
        .filter(|module| module.skipped)
        .map(|module| module.path.as_str())
        .collect();
    let reachable_dirs = reachable_dirs(graph, &reachable);
    let path_refs = path_refs(graph, &reachable, parse);
    let mut unreachable = Vec::new();
    let mut unread = Vec::new();
    for file in &sources.files {
        if !matches!(file.kind, FileKind::Python | FileKind::Notebook) {
            continue;
        }
        if production && !file.context.is_included_in_production() {
            continue;
        }
        let mode = entry.mode_for(&file.path);
        if is_excluded(file, mode) {
            continue;
        }
        let Some(file_id) = graph.file_id(&file.path) else {
            continue;
        };
        if reachable.contains(&file_id) || is_path_referenced(&file.path, &path_refs) {
            continue;
        }

        let orphan = UnreachableFile {
            file: file_id,
            path: file.path.clone(),
            max_confidence: confidence_for_unreachable(mode),
            mode,
            confidence_reason: None,
            test_fixture: is_test_fixture(file, &reachable_dirs),
            config_mention: None,
        };
        if skipped.contains(file.path.as_str()) {
            unread.push(orphan);
        } else {
            unreachable.push(orphan);
        }
    }

    unreachable.sort_by(|left, right| left.path.cmp(&right.path));
    unread.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(ReachabilityReport {
        reachable,
        eager: bfs.eager,
        exposed: bfs.exposed,
        certain: bfs.certain,
        unreachable,
        unread,
        used_modules: bfs.used_modules,
        framework_used: framework.files,
        predecessors: bfs.predecessors,
        plugin_ref_files,
        listed_files: bfs.listed,
    })
}

/// Re-score library-mode orphans that the wheel does not ship (R-05).
///
/// Library mode caps orphans at `Maybe` because an outside caller may import
/// them; a file outside the distributed packages has no such caller, so it is
/// scored as in app mode ([`EntryPlan::surface_mode`]). The surface is the
/// root distribution's, so files of library workspace members, which ship in
/// their own wheels, keep `Maybe`.
pub fn apply_public_surface(
    report: &mut ReachabilityReport,
    surface: &PublicSurface,
    entry: &EntryPlan,
) {
    for file in report.unreachable.iter_mut().chain(&mut report.unread) {
        let mode = entry.surface_mode(&file.path, Some(surface));
        if mode != file.mode {
            score_as_app(file);
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
    for file in report.unreachable.iter_mut().chain(&mut report.unread) {
        if member_ships(surfaces.iter().copied(), &file.path) == Some(false) {
            score_as_app(file);
        }
    }
}

/// Re-score app orphans that the wheel of a member a metapackage root
/// distributes ships as library orphans (#1060).
///
/// The member stays an app, so CHK006 / CHK007 keep app severity, but its
/// users install that wheel and may import any file it ships: as for the
/// wheel-shipped `__all__` of an app member (§8), only the CHK001 scoring
/// changes. `surfaces` pairs each such member's root-relative directory and
/// member-relative surface with the line that names the requirement. A
/// library member nested inside is judged by its own surface.
pub(crate) fn apply_distributed_surfaces(
    report: &mut ReachabilityReport,
    surfaces: &[(&(String, PublicSurface), &str)],
    entry: &EntryPlan,
) {
    for file in report.unreachable.iter_mut().chain(&mut report.unread) {
        if entry.in_library_member(&file.path) {
            continue;
        }
        let shipping = surfaces
            .iter()
            .filter_map(|((member, surface), reason)| {
                let rel = file.path.strip_prefix(member.as_str())?.strip_prefix('/')?;
                Some((member.len(), surface.contains(rel), *reason))
            })
            .max_by_key(|(len, ..)| *len);
        if let Some((_, true, reason)) = shipping {
            file.mode = ProjectMode::Library;
            file.max_confidence = confidence_for_unreachable(ProjectMode::Library);
            file.confidence_reason = Some(reason.to_owned());
        }
    }
}

/// A shipped-surface re-score: the file has no outside caller, so it is
/// judged and capped as in app mode.
fn score_as_app(file: &mut UnreachableFile) {
    file.mode = ProjectMode::App;
    file.max_confidence = confidence_for_unreachable(ProjectMode::App);
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

/// The directories that hold a reachable file.
fn reachable_dirs<'g>(
    graph: &'g ProjectGraph,
    reachable: &IndexSet<crate::graph::FileId>,
) -> std::collections::HashSet<&'g str> {
    reachable
        .iter()
        .filter_map(|id| graph.file(*id))
        .map(|file| parent_rel(&file.path))
        .collect()
}

/// The `.py` paths reachable modules hand to a path or file call (#1274).
fn path_refs<'p>(
    graph: &ProjectGraph,
    reachable: &IndexSet<FileId>,
    parse: &'p ParseSummary,
) -> Vec<&'p str> {
    let mut refs: Vec<&str> = parse
        .modules
        .iter()
        .filter(|module| {
            !module.path_refs.is_empty()
                && graph
                    .file_id(&module.path)
                    .is_some_and(|file_id| reachable.contains(&file_id))
        })
        .flat_map(|module| module.path_refs.iter().map(String::as_str))
        .collect();
    refs.sort_unstable();
    refs.dedup();
    refs
}

/// Whether a reachable module reads, copies or runs the file by a path its
/// own trailing segments spell (`os.path.join(data, "fixtures/ir.py")`), so
/// it is used though nothing imports it (#1274). Its imports are not
/// followed: such a file is copied or run in another tree (a test driver,
/// a builtins stub).
fn is_path_referenced(path: &str, refs: &[&str]) -> bool {
    refs.iter().any(|reference| {
        path.strip_suffix(reference)
            .is_some_and(|head| head.is_empty() || head.ends_with('/'))
    })
}

/// Whether an unreachable file is test input a harness loads by path: a
/// test-context file that is neither a conftest nor named like a test
/// module, in a directory no reachable file shares (#1268).
fn is_test_fixture(
    file: &crate::sources::DiscoveredFile,
    reachable_dirs: &std::collections::HashSet<&str>,
) -> bool {
    file.context == FileContext::Test
        && !reachable_dirs.contains(parent_rel(&file.path))
        && !is_test_harness_name(file_name(&file.path))
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn is_excluded(file: &crate::sources::DiscoveredFile, mode: ProjectMode) -> bool {
    file.path.ends_with("__init__.py")
        || (file.context == FileContext::Test && mode == ProjectMode::Library)
}

/// A library orphan may still be imported by an outside caller. Unread input
/// in reachable code lowers orphans later, in the gap stage (ADR 0005).
const fn confidence_for_unreachable(mode: ProjectMode) -> Confidence {
    if matches!(mode, ProjectMode::Library) {
        return Confidence::Maybe;
    }
    Confidence::Certain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExitStatus;
    use crate::config::PluginId;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::entry::EntryRoot;
    use crate::graph::{FileNode, ModuleOrigin, add_parsed_imports};
    use crate::parser::{ImportContext, ImportKind, ImportRef, ParsedModule};
    use crate::plugins::{FrameworkUsedGlob, PluginContribution, ReferenceOrigin};
    use crate::reachability::trace_to_file;
    use crate::rules::Severity;
    use crate::sources::{DiscoveredFile, LayoutInfo};
    use std::collections::BTreeSet;

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
            config_requirement_names: Vec::new(),
            unread_requirements: Vec::new(),
            config_module_refs: Vec::new(),
            config_used_symbols: Vec::new(),
            config_script_entries: Vec::new(),
            config_command_targets: Vec::new(),
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
                    fallback_for: Vec::new(),
                    version_minors: None,
                    called_before: Vec::new(),
                    tested_before: Vec::new(),
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
            distributed_members: Vec::new(),
            roots: roots
                .iter()
                .map(|path| EntryRoot {
                    path: (*path).to_owned(),
                    symbols: BTreeSet::new(),
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
            layout: LayoutInfo::new("", vec!["acme".to_owned()]),
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

    /// A test-context orphan is test input only where no reachable file
    /// shares its directory and it is not named like a test module (#1268).
    #[test]
    fn test_orphans_away_from_reachable_files_are_fixtures() {
        let paths = [
            "acme/main.py",
            "acme/orphan.py",
            "tests/test_main.py",
            "tests/helpers_old.py",
            "tests/targets/demo/plugins/action.py",
            "tests/targets/demo/test_shape.py",
            "tests/targets/demo/conftest.py",
        ];
        let parse = ParseSummary {
            modules: paths.map(|path| parsed(path, &[], false)).into(),
        };
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        };
        let mut sources = flat_sources(root, &paths);
        for file in &mut sources.files {
            if file.path.starts_with("tests/") {
                file.context = FileContext::Test;
            }
        }
        let mut graph = graph_for(&sources, &parse, &[]);
        let entry = app_entry(&["acme/main.py", "tests/test_main.py"]);
        let report =
            analyze_reachability(&mut graph, &sources, &entry, &no_plugins(), &parse, false)
                .expect("reachability");

        let found: Vec<_> = report
            .unreachable
            .iter()
            .map(|file| (file.path.as_str(), file.test_fixture))
            .collect();
        assert_eq!(
            found,
            [
                ("acme/orphan.py", false),
                ("tests/helpers_old.py", false),
                ("tests/targets/demo/conftest.py", false),
                ("tests/targets/demo/plugins/action.py", true),
                ("tests/targets/demo/test_shape.py", false),
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
        assert!(!report.unreachable[0].callers_may_import());
    }

    /// A skipped orphan is no CHK001 finding, but it is scored like one: a
    /// public one may still be imported by a caller of the library, which
    /// the gap stage needs to know (#550).
    #[test]
    fn skipped_orphans_are_scored_apart_from_reported_ones() {
        let skipped = |path: &str| ParsedModule {
            path: path.to_owned(),
            skipped: true,
            ..ParsedModule::default()
        };
        let parse = ParseSummary {
            modules: vec![
                parsed("acme/__init__.py", &[], false),
                skipped("acme/client.py"),
                skipped("tools/release.py"),
            ],
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
        let mut report =
            analyze_reachability(&mut graph, &sources, &entry, &no_plugins(), &parse, false)
                .expect("reachability");
        let surface = PublicSurface {
            files: ["acme/client.py".to_owned()].into(),
        };
        apply_public_surface(&mut report, &surface, &entry);

        assert_eq!(report.unreachable, []);
        let unread: Vec<_> = report
            .unread
            .iter()
            .map(|file| (file.path.as_str(), file.callers_may_import()))
            .collect();
        assert_eq!(
            unread,
            [("acme/client.py", true), ("tools/release.py", false)]
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
    fn distributed_member_lowers_only_orphans_its_wheel_ships() {
        let parse = ParseSummary {
            modules: [
                "acme/main.py",
                "slim/pkg/orphan.py",
                "slim/scripts/release.py",
                "slim/libs/core/pkg/orphan.py",
                "tools/orphan.py",
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
        entry.library_members = vec!["slim/libs/core".to_owned()];
        let mut report =
            analyze_reachability(&mut graph, &sources, &entry, &no_plugins(), &parse, false)
                .expect("reachability");
        let core = (
            "slim/libs/core".to_owned(),
            PublicSurface {
                files: ["other.py".to_owned()].into(),
            },
        );
        apply_member_surfaces(&mut report, &[&core]);
        let slim = (
            "slim".to_owned(),
            PublicSurface {
                files: [
                    "pkg/orphan.py".to_owned(),
                    "libs/core/pkg/orphan.py".to_owned(),
                ]
                .into(),
            },
        );
        apply_distributed_surfaces(&mut report, &[(&slim, "confidence: library (why)")], &entry);

        let found: Vec<_> = report
            .unreachable
            .iter()
            .map(|file| {
                (
                    file.path.as_str(),
                    file.max_confidence,
                    file.callers_may_import(),
                    file.confidence_reason.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (
                    "slim/libs/core/pkg/orphan.py",
                    Confidence::Certain,
                    false,
                    None
                ),
                (
                    "slim/pkg/orphan.py",
                    Confidence::Maybe,
                    true,
                    Some("confidence: library (why)")
                ),
                ("slim/scripts/release.py", Confidence::Certain, false, None),
                ("tools/orphan.py", Confidence::Certain, false, None),
            ]
        );
    }

    /// One orphan through scoring, the wheel surfaces, the gap stage, and
    /// emit, in each mode and place it can be in.
    #[derive(Debug, Clone, Copy)]
    enum OrphanPlace {
        AppRoot,
        LibraryRoot,
        MemberOfApp,
        MemberOfLibrary,
        /// An app member of an app root, which ships no wheel of its own
        /// unless a metapackage root distributes it (#1060).
        AppMember,
    }

    impl OrphanPlace {
        const fn library_root(self) -> bool {
            matches!(self, Self::LibraryRoot | Self::MemberOfLibrary)
        }

        const fn in_member(self) -> bool {
            matches!(
                self,
                Self::MemberOfApp | Self::MemberOfLibrary | Self::AppMember
            )
        }

        const fn library_member(self) -> bool {
            matches!(self, Self::MemberOfApp | Self::MemberOfLibrary)
        }
    }

    #[expect(
        clippy::struct_excessive_bools,
        reason = "independent switches of one table row"
    )]
    struct OrphanCase {
        place: OrphanPlace,
        /// The root wheel surface, if declared: whether it ships the orphan.
        root_ships: Option<bool>,
        /// The member wheel surface, if declared: whether it ships the orphan.
        member_ships: Option<bool>,
        /// Whether the root is a metapackage that distributes the member.
        distributed: bool,
        opaque: bool,
        shim: bool,
        strict: bool,
    }

    impl OrphanCase {
        fn all() -> Vec<Self> {
            let flags = [false, true];
            let surfaces = [None, Some(false), Some(true)];
            let mut cases = Vec::new();
            let places = [
                OrphanPlace::AppRoot,
                OrphanPlace::LibraryRoot,
                OrphanPlace::MemberOfApp,
                OrphanPlace::MemberOfLibrary,
                OrphanPlace::AppMember,
            ];
            let ships = surfaces.into_iter().flat_map(|root| {
                surfaces
                    .into_iter()
                    .flat_map(move |member| flags.map(|distributed| (root, member, distributed)))
            });
            let switches = flags.into_iter().flat_map(|opaque| {
                flags
                    .into_iter()
                    .flat_map(move |shim| flags.map(|strict| (opaque, shim, strict)))
            });
            for place in places {
                // A metapackage root distributes members; a root file has none.
                let ships = ships
                    .clone()
                    .filter(|&(_, _, distributed)| place.in_member() || !distributed);
                for (root_ships, member_ships, distributed) in ships {
                    for (opaque, shim, strict) in switches.clone() {
                        cases.push(Self {
                            place,
                            root_ships,
                            member_ships,
                            distributed,
                            opaque,
                            shim,
                            strict,
                        });
                    }
                }
            }
            cases
        }

        fn orphan(&self) -> &'static str {
            if self.place.in_member() {
                "libs/core/pkg/orphan.py"
            } else {
                "acme/orphan.py"
            }
        }

        fn run(&self) -> (Option<(Severity, Confidence)>, ExitStatus) {
            let orphan = self.orphan();
            let mut orphan_module = parsed(orphan, &[], false);
            orphan_module.deprecated_shim = self.shim;
            let parse = ParseSummary {
                modules: vec![parsed("acme/main.py", &[], self.opaque), orphan_module],
            };
            let root = ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            };
            let sources = flat_sources(root, &["acme/main.py", orphan]);
            let mut graph = graph_for(&sources, &parse, &[]);
            let mut entry = app_entry(&["acme/main.py"]);
            if self.place.library_root() {
                entry.mode = ProjectMode::Library;
            }
            if self.place.library_member() {
                entry.library_members = vec!["libs/core".to_owned()];
            }
            let mut report =
                analyze_reachability(&mut graph, &sources, &entry, &no_plugins(), &parse, false)
                    .expect("reachability");
            let surface = |ships: bool, path: &str| PublicSurface {
                files: [if ships { path } else { "elsewhere.py" }.to_owned()].into(),
            };
            if let Some(ships) = self.root_ships {
                apply_public_surface(&mut report, &surface(ships, orphan), &entry);
            }
            // As in the pipeline: only a library member's declared wheel
            // narrows reachability, and a distributed member without one
            // ships its layout's packages, the orphan among them.
            let member = self
                .member_ships
                .map(|ships| ("libs/core".to_owned(), surface(ships, "pkg/orphan.py")));
            if self.place.library_member() {
                apply_member_surfaces(&mut report, &member.iter().collect::<Vec<_>>());
            }
            if self.distributed {
                let shipped = member
                    .unwrap_or_else(|| ("libs/core".to_owned(), surface(true, "pkg/orphan.py")));
                apply_distributed_surfaces(&mut report, &[(&shipped, "distributed")], &entry);
            }

            let gaps: Vec<_> = self
                .opaque
                .then(|| crate::rules::Gap {
                    file: "acme/main.py".to_owned(),
                    kind: crate::rules::GapKind::OpaqueDynamicImport,
                    member: None,
                    site: None,
                    target: None,
                })
                .into_iter()
                .collect();
            let issues = crate::rules::emit_issues(
                &report,
                &crate::rules::DependencyReport::default(),
                &[],
                &parse,
                &gaps,
                &crate::config::default_config(),
                &crate::config::RuntimeOverrides {
                    strict: Some(self.strict),
                    ..Default::default()
                },
                &crate::resolver::ResolutionIndex::default(),
            );
            let finding = issues
                .issues
                .iter()
                .find(|issue| issue.rule == crate::rules::RuleId::Chk001)
                .map(|issue| (issue.severity, issue.confidence));
            (finding, issues.exit_status)
        }

        /// §8 / §11 written out: a file of a library that its wheel ships
        /// (or every file, when no wheel target is declared) may be imported
        /// by a caller, so it is `maybe`, and so is a file of an app member
        /// that a metapackage root distributes, when the member's wheel ships
        /// it (#1060, #1073); a reachable module read only in
        /// part lowers the rest to `likely`; a deprecated shim is info and
        /// every other candidate a warning (§3); and the exit status counts
        /// what the run counts (§7, E1).
        fn expected(&self) -> (Option<(Severity, Confidence)>, ExitStatus) {
            let library = self.place.library_root()
                || self.place.library_member()
                || (self.place.in_member() && self.distributed);
            let ships = if self.place.in_member() {
                self.member_ships
            } else {
                self.root_ships
            };
            let callers_may_import = library && ships != Some(false);
            let confidence = if callers_may_import {
                Confidence::Maybe
            } else if self.opaque {
                Confidence::Likely
            } else {
                Confidence::Certain
            };
            let severity = if self.shim {
                Severity::Info
            } else {
                Severity::Warning
            };
            let floor = if self.strict {
                Confidence::Maybe
            } else {
                Confidence::Likely
            };
            let finding = (confidence >= floor).then_some((severity, confidence));
            let fails = finding.is_some_and(|(severity, _)| {
                severity == Severity::Error || (self.strict && severity == Severity::Warning)
            });
            let status = if fails {
                ExitStatus::IssuesFound
            } else {
                ExitStatus::Success
            };
            (finding, status)
        }
    }

    /// §8 CHK001 confidence and severity, through every stage that sets them,
    /// against a reference model: in particular a file a caller of the
    /// library may import never fails a run outside `--strict`, so the E1
    /// exit status does not change with it (#860).
    #[test]
    fn library_orphans_callers_may_import_never_fail_the_run() {
        for case in OrphanCase::all() {
            let label = format!(
                "{:?}, root ships {:?}, member ships {:?}, distributed {}, opaque {}, shim {}, strict {}",
                case.place,
                case.root_ships,
                case.member_ships,
                case.distributed,
                case.opaque,
                case.shim,
                case.strict
            );
            assert_eq!(case.run(), case.expected(), "{label}");
        }
    }

    #[test]
    fn skipped_file_is_not_reported_and_leaves_orphans_to_the_gap_stage() {
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
        assert_eq!(unreachable, [("acme/orphan.py", Confidence::Certain)]);
    }
}
