//! Breadth-first reachability traversal.

use std::collections::{HashMap, HashSet, VecDeque};

use indexmap::{IndexMap, IndexSet};

use crate::config::ProjectMode;
use crate::entry::EntryPlan;
use crate::graph::{FileId, GraphEdge, ModuleId, ModuleOrigin, ProjectGraph};
use crate::parser::{ImportContext, ParseSummary};
use crate::plugins::PluginHints;
use crate::resolver::import_root;
use crate::sources::FileContext;

use super::module_index::ModuleIndex;
use super::types::{ReachPredecessor, TraceStep, UsedModule};

/// Result of a BFS traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BfsOutcome {
    /// Files reached from entry roots, plugin refs, framework globs, and imports.
    pub reachable: IndexSet<FileId>,
    /// Shortest-path predecessors for trace reconstruction.
    pub predecessors: IndexMap<FileId, ReachPredecessor>,
    /// Stdlib and third-party modules encountered.
    pub used_modules: Vec<UsedModule>,
    /// Reachable files that some path reaches without passing a
    /// function-local or `TYPE_CHECKING` import, so they load as soon as
    /// their entry does. Imports out of test, docs and dev files reach only
    /// library files: loading from those says nothing about runtime (#614).
    pub eager: IndexSet<FileId>,
    /// Eager files that some path reaches without passing an optional
    /// (`try`/`suppress(ImportError)` or platform-guarded) import either.
    pub certain: IndexSet<FileId>,
}

/// One import site on a file: the module it names and the line it sits on.
type ImportSite = (ModuleId, u32);

/// A `from pkg import name` site where `pkg.name` is itself a first-party
/// module: the resolved file, the dotted submodule name, and the line.
type SubmoduleSite = (FileId, String, u32);

/// An import site tagged with the file it appears in.
type ImportSiteRef = (FileId, ModuleId, u32);

/// A file and one of its lines.
type FileLine = (FileId, u32);

/// How surely a reached file loads, ordered weakest first. A file's class is
/// the best any path to it gives; an edge passes on at most its source's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reach {
    /// Reached only through a lazy import, or out of a test, docs or dev
    /// file into a non-library file.
    Reachable,
    /// Reached without lazy imports, but only through optional ones.
    Eager,
    /// Reached through imports that always run.
    Certain,
}

/// A reached file: its best class so far and the class its imports were last
/// followed at, if they have been.
#[derive(Debug, Clone, Copy)]
struct Visit {
    class: Reach,
    followed: Option<Reach>,
}

struct Adjacency {
    file_imports: HashMap<FileId, Vec<ImportSite>>,
    submodule_imports: HashMap<FileId, Vec<SubmoduleSite>>,
    prefix_imports: HashMap<FileId, Vec<SubmoduleSite>>,
    dynamic_sites: HashSet<ImportSiteRef>,
    /// Function-local and `TYPE_CHECKING` import lines.
    lazy: HashSet<FileLine>,
    /// `try`/`suppress(ImportError)` and platform-guarded import lines.
    optional: HashSet<FileLine>,
}

struct BfsState<'a> {
    // Shared, so module and file names can be borrowed for the whole walk and
    // copied only into the steps of newly reached files.
    graph: &'a ProjectGraph,
    module_index: &'a ModuleIndex,
    entry: &'a EntryPlan,
    adjacency: &'a Adjacency,
    queue: VecDeque<FileId>,
    reached: IndexMap<FileId, Visit>,
    predecessors: IndexMap<FileId, ReachPredecessor>,
    used_modules: Vec<UsedModule>,
}

impl BfsState<'_> {
    /// Imports out of test, docs and dev files only count for library files,
    /// which outside callers may import directly.
    fn edge_class(&self, from: FileId, class: Reach, line: u32, target: FileId) -> Reach {
        if class == Reach::Reachable || self.adjacency.lazy.contains(&(from, line)) {
            return Reach::Reachable;
        }
        let file = |id| self.graph.file(id);
        let runtime = file(from).is_none_or(|node| node.context == FileContext::Runtime)
            || file(target)
                .is_some_and(|node| self.entry.mode_for(&node.path) == ProjectMode::Library);
        if !runtime {
            Reach::Reachable
        } else if self.adjacency.optional.contains(&(from, line)) {
            class.min(Reach::Eager)
        } else {
            class
        }
    }

    /// Mark `file_id` reached at `class`; `step` is only built for a file
    /// seen the first time. A file reached again at a better class is
    /// queued again so its imports pass that class on.
    fn enqueue_file(
        &mut self,
        file_id: FileId,
        from: Option<FileId>,
        class: Reach,
        step: impl FnOnce() -> TraceStep,
    ) {
        match self.reached.entry(file_id) {
            indexmap::map::Entry::Vacant(vacant) => {
                vacant.insert(Visit {
                    class,
                    followed: None,
                });
                self.predecessors
                    .insert(file_id, ReachPredecessor { from, step: step() });
                self.queue.push_back(file_id);
            },
            indexmap::map::Entry::Occupied(mut occupied) => {
                let visit = occupied.get_mut();
                if class > visit.class {
                    visit.class = class;
                    self.queue.push_back(file_id);
                }
            },
        }
    }

    fn finish(self) -> BfsOutcome {
        let at_least = |floor: Reach| -> IndexSet<FileId> {
            self.reached
                .iter()
                .filter(|(_, visit)| visit.class >= floor)
                .map(|(file_id, _)| *file_id)
                .collect()
        };
        let eager = at_least(Reach::Eager);
        let certain = at_least(Reach::Certain);
        BfsOutcome {
            reachable: self.reached.keys().copied().collect(),
            predecessors: self.predecessors,
            used_modules: self.used_modules,
            eager,
            certain,
        }
    }

    fn drain(&mut self) {
        while let Some(file_id) = self.queue.pop_front() {
            let visit = &mut self.reached[&file_id];
            let first = visit.followed.is_none();
            if visit
                .followed
                .is_some_and(|followed| followed >= visit.class)
            {
                continue;
            }
            visit.followed = Some(visit.class);
            let class = visit.class;
            record_file_imports(self, file_id, class, first);
        }
    }
}

/// Run BFS from entry roots through first-party import edges.
///
/// One walk computes every set: each file carries the best [`Reach`] class
/// some path gives it, and is followed again only when that class improves,
/// so no file is followed more than three times.
///
/// Framework-glob files are seeded after the walk from the entry roots and
/// plugin refs has finished, so a file those reach keeps its import trace;
/// the imports of the seeded files are then followed like any other file's.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_reachability_bfs(
    graph: &ProjectGraph,
    entry: &EntryPlan,
    plugins: &PluginHints,
    parse: &ParseSummary,
    module_index: &ModuleIndex,
    framework: Vec<(FileId, ReachPredecessor)>,
) -> BfsOutcome {
    let (lazy, optional) = conditional_lines(graph, parse);
    let adjacency = Adjacency {
        file_imports: build_file_import_adjacency(graph),
        submodule_imports: build_submodule_imports(graph, parse, module_index),
        prefix_imports: build_prefix_imports(graph, parse, module_index),
        dynamic_sites: build_dynamic_sites(graph, parse),
        lazy,
        optional,
    };
    let mut state = BfsState {
        graph,
        module_index,
        entry,
        adjacency: &adjacency,
        queue: VecDeque::new(),
        reached: IndexMap::new(),
        predecessors: IndexMap::new(),
        used_modules: Vec::new(),
    };

    for root in &entry.roots {
        let Some(file_id) = state.graph.file_id(&root.spec.path) else {
            continue;
        };
        state.enqueue_file(file_id, None, Reach::Certain, || TraceStep::File {
            file: file_id,
            path: root.spec.path.clone(),
        });
    }
    for reference in plugins.module_refs() {
        let label = reference.origin.label.as_str();
        for name in module_and_parents(&reference.module) {
            let Some(target) = state.module_index.resolve(name) else {
                continue;
            };
            state.enqueue_file(target, None, Reach::Certain, || TraceStep::PluginRef {
                module: name.to_owned(),
                label: label.to_owned(),
            });
        }
    }
    state.drain();

    for (file_id, ReachPredecessor { from, step }) in framework {
        state.enqueue_file(file_id, from, Reach::Certain, || step);
    }
    state.drain();

    state.finish()
}

/// Stdlib and third-party uses are recorded only on the `first` visit; later
/// visits only pass on a better class to files already reached.
fn record_file_imports(state: &mut BfsState<'_>, file_id: FileId, class: Reach, first: bool) {
    let graph = state.graph;
    let adjacency = state.adjacency;
    let source_path = graph.file(file_id).map_or("", |node| node.path.as_str());

    for &(module_id, line) in adjacency.file_imports.get(&file_id).into_iter().flatten() {
        let Some(module_node) = graph.module(module_id) else {
            continue;
        };
        let name = module_node.name.as_str();
        match module_node.origin {
            ModuleOrigin::FirstParty => {
                let dynamic = adjacency
                    .dynamic_sites
                    .contains(&(file_id, module_id, line));
                enqueue_import(state, file_id, class, name, line, dynamic);
            },
            origin @ (ModuleOrigin::Stdlib | ModuleOrigin::ThirdParty) if first => {
                state.used_modules.push(UsedModule {
                    full_module: name.to_owned(),
                    import_root: import_root(name).to_owned(),
                    origin,
                    file: source_path.to_owned(),
                    line,
                });
            },
            ModuleOrigin::Stdlib | ModuleOrigin::ThirdParty | ModuleOrigin::Unknown => {},
        }
    }

    for (target, module, line) in adjacency
        .submodule_imports
        .get(&file_id)
        .into_iter()
        .flatten()
    {
        let edge = state.edge_class(file_id, class, *line, *target);
        state.enqueue_file(*target, Some(file_id), edge, || TraceStep::Import {
            module: module.clone(),
            line: *line,
        });
    }

    for (target, module, line) in adjacency.prefix_imports.get(&file_id).into_iter().flatten() {
        let edge = state.edge_class(file_id, class, *line, *target);
        state.enqueue_file(*target, Some(file_id), edge, || TraceStep::DynamicImport {
            module: module.clone(),
            line: *line,
        });
    }
}

/// Importing `pkg.sub.mod` runs `pkg/__init__.py` and `pkg/sub/__init__.py`
/// before `mod`, so every dotted prefix that resolves is reached from the
/// same site.
#[allow(clippy::too_many_arguments)]
fn enqueue_import(
    state: &mut BfsState<'_>,
    from_file: FileId,
    class: Reach,
    module: &str,
    line: u32,
    dynamic: bool,
) {
    let from_path = state
        .graph
        .file(from_file)
        .map_or("", |node| node.path.as_str());
    for name in module_and_parents(module) {
        let Some(target) = state.module_index.resolve_from(from_path, name) else {
            continue;
        };
        let edge = state.edge_class(from_file, class, line, target);
        state.enqueue_file(target, Some(from_file), edge, || {
            let module = name.to_owned();
            if dynamic {
                TraceStep::DynamicImport { module, line }
            } else {
                TraceStep::Import { module, line }
            }
        });
    }
}

/// `module` itself, then each enclosing package from the outermost in.
fn module_and_parents(module: &str) -> impl Iterator<Item = &str> {
    std::iter::once(module).chain(
        module
            .match_indices('.')
            .filter_map(move |(index, _)| module.get(..index)),
    )
}

fn build_file_import_adjacency(graph: &ProjectGraph) -> HashMap<FileId, Vec<ImportSite>> {
    let mut adjacency: HashMap<FileId, Vec<ImportSite>> = HashMap::new();
    for edge in graph.edges() {
        if let GraphEdge::FileImportsModule { file, module, line } = edge {
            adjacency.entry(*file).or_default().push((*module, *line));
        }
    }
    adjacency
}

/// `from pkg import name` loads `pkg.name` when that is a submodule, so each
/// such site reaches the submodule's file as well as `pkg` itself.
fn build_submodule_imports(
    graph: &ProjectGraph,
    parse: &ParseSummary,
    module_index: &ModuleIndex,
) -> HashMap<FileId, Vec<SubmoduleSite>> {
    let mut sites: HashMap<FileId, Vec<SubmoduleSite>> = HashMap::new();
    for module in &parse.modules {
        let Some(file_id) = graph.file_id(&module.path) else {
            continue;
        };
        for import in &module.imports {
            let Some(name) = import.name.as_deref() else {
                continue;
            };
            if import.module.is_empty() {
                continue;
            }
            let submodule = format!("{}.{name}", import.module);
            if let Some(target) = module_index.resolve_from(&module.path, &submodule) {
                sites
                    .entry(file_id)
                    .or_default()
                    .push((target, submodule, import.line));
            }
        }
    }
    sites
}

/// `import_module("pkg.commands." + name)` may load any module under
/// `pkg.commands`, so each such site reaches all of them.
fn build_prefix_imports(
    graph: &ProjectGraph,
    parse: &ParseSummary,
    module_index: &ModuleIndex,
) -> HashMap<FileId, Vec<SubmoduleSite>> {
    let mut sites: HashMap<FileId, Vec<SubmoduleSite>> = HashMap::new();
    for module in &parse.modules {
        let Some(file_id) = graph.file_id(&module.path) else {
            continue;
        };
        for prefix in &module.dynamic_import_prefixes {
            for (name, target) in module_index.under(&prefix.module) {
                sites
                    .entry(file_id)
                    .or_default()
                    .push((target, name.to_owned(), prefix.line));
            }
        }
    }
    sites
}

/// Import sites that came from `importlib.import_module("m")` rather than an
/// `import` statement. Both kinds share one `FileImportsModule` edge, so the
/// parse summary is what distinguishes them. A site that is also a static
/// import on the same line stays static.
fn build_dynamic_sites(graph: &ProjectGraph, parse: &ParseSummary) -> HashSet<ImportSiteRef> {
    let mut sites = HashSet::new();
    for module in &parse.modules {
        let Some(file_id) = graph.file_id(&module.path) else {
            continue;
        };
        for dynamic in module.dynamic_imports.iter().chain(&module.pytest_plugins) {
            let also_static = module
                .imports
                .iter()
                .any(|import| import.module == dynamic.module && import.line == dynamic.line);
            if also_static {
                continue;
            }
            if let Some(module_id) = graph.module_id(&dynamic.module) {
                sites.insert((file_id, module_id, dynamic.line));
            }
        }
    }
    sites
}

/// Lines whose import may not run when the file loads: lazy ones
/// (function-local imports and `TYPE_CHECKING` blocks) and optional ones
/// (under `try`/`suppress(ImportError)` or a platform guard). None of these
/// shares a line with an import outside it, so the line alone identifies the
/// site.
fn conditional_lines(
    graph: &ProjectGraph,
    parse: &ParseSummary,
) -> (HashSet<FileLine>, HashSet<FileLine>) {
    let mut lazy = HashSet::new();
    let mut optional = HashSet::new();
    for module in &parse.modules {
        let Some(file_id) = graph.file_id(&module.path) else {
            continue;
        };
        let static_sites = module.imports.iter().map(|import| {
            (
                import.line,
                import.deferred || import.context == ImportContext::Type,
                import.optional || import.platform_guarded,
            )
        });
        let dynamic_sites = module
            .dynamic_imports
            .iter()
            .chain(&module.dynamic_import_prefixes)
            .map(|dynamic| {
                (
                    dynamic.line,
                    dynamic.deferred,
                    dynamic.optional || dynamic.platform_guarded,
                )
            });
        for (line, is_lazy, is_optional) in static_sites.chain(dynamic_sites) {
            if is_lazy {
                lazy.insert((file_id, line));
            }
            if is_optional {
                optional.insert((file_id, line));
            }
        }
    }
    (lazy, optional)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EntrySpec, ProjectMode};
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::entry::EntryRoot;
    use crate::graph::{FileNode, add_parsed_imports};
    use crate::parser::{DynamicImport, ImportContext, ImportKind, ImportRef, ParsedModule};
    use crate::sources::{
        DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, ProjectLayout,
    };

    const PATHS: [&str; 4] = [
        "src/acme/main.py",
        "src/acme/a.py",
        "src/acme/b.py",
        "src/acme/c.py",
    ];

    fn parsed(path: &str, imports: &[(&str, u32)], dynamic: &[(&str, u32)]) -> ParsedModule {
        ParsedModule {
            path: path.to_owned(),
            imports: imports
                .iter()
                .map(|(module, line)| ImportRef {
                    module: (*module).to_owned(),
                    name: None,
                    alias: None,
                    line: *line,
                    kind: ImportKind::Import,
                    context: ImportContext::Runtime,
                    optional: false,
                    platform_guarded: false,
                    deferred: false,
                    relative_level: 0,
                })
                .collect(),
            dynamic_imports: dynamic
                .iter()
                .map(|(module, line)| DynamicImport {
                    module: (*module).to_owned(),
                    line: *line,
                    ..DynamicImport::default()
                })
                .collect(),
            ..ParsedModule::default()
        }
    }

    fn layout() -> LayoutInfo {
        LayoutInfo {
            layout: ProjectLayout::Src,
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            ..Default::default()
        }
    }

    fn sources(root: &ProjectRoot) -> DiscoveredSources {
        DiscoveredSources {
            root: root.clone(),
            layout: layout(),
            effective_globs: Vec::new(),
            files: PATHS
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

    fn entry_plan() -> EntryPlan {
        EntryPlan {
            mode: ProjectMode::App,
            library_members: Vec::new(),
            roots: vec![EntryRoot {
                spec: EntrySpec {
                    path: "src/acme/main.py".to_owned(),
                    symbol: None,
                },
                context: FileContext::Runtime,
                origins: Vec::new(),
            }],
            warnings: Vec::new(),
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

    fn graph_with_imports(root: ProjectRoot, modules: &[ParsedModule]) -> ProjectGraph {
        graph_with_files(root, &PATHS, &["acme.a", "acme.b", "acme.c"], modules)
    }

    fn graph_with_files(
        root: ProjectRoot,
        paths: &[&str],
        first_party: &[&str],
        modules: &[ParsedModule],
    ) -> ProjectGraph {
        fill_graph(ProjectGraph::new(root), paths, first_party, modules)
    }

    fn fill_graph(
        mut graph: ProjectGraph,
        paths: &[&str],
        first_party: &[&str],
        modules: &[ParsedModule],
    ) -> ProjectGraph {
        for path in paths {
            graph
                .intern_file(FileNode {
                    path: (*path).to_owned(),
                    context: FileContext::Runtime,
                    kind: FileKind::Python,
                })
                .expect("file");
        }
        for module in first_party {
            graph.intern_module((*module).to_owned(), ModuleOrigin::FirstParty);
        }
        for parsed in modules {
            let file_id = graph.file_id(&parsed.path).expect("parsed file");
            add_parsed_imports(&mut graph, file_id, parsed).expect("import edges");
        }
        graph
    }

    fn test_root() -> ProjectRoot {
        ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        }
    }

    fn run_bfs(
        graph: &ProjectGraph,
        root: &ProjectRoot,
        modules: Vec<ParsedModule>,
        framework: Vec<(FileId, ReachPredecessor)>,
    ) -> BfsOutcome {
        let parse = ParseSummary { modules };
        let module_index = ModuleIndex::build(graph, &sources(root));
        run_reachability_bfs(
            graph,
            &entry_plan(),
            &no_plugins(),
            &parse,
            &module_index,
            framework,
        )
    }

    fn reached_from(outcome: &BfsOutcome, file_id: FileId) -> Option<FileId> {
        outcome
            .predecessors
            .get(&file_id)
            .and_then(|pred| pred.from)
    }

    #[test]
    fn dynamic_import_reach_is_kept_dynamic() {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        };
        let modules = vec![
            parsed("src/acme/main.py", &[("acme.a", 1)], &[("acme.b", 2)]),
            parsed("src/acme/b.py", &[], &[("acme.c", 3)]),
        ];
        let graph = graph_with_imports(root.clone(), &modules);
        let parse = ParseSummary { modules };

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        let b_id = graph.file_id("src/acme/b.py").expect("b");
        let c_id = graph.file_id("src/acme/c.py").expect("c");
        let module_index = ModuleIndex::build(&graph, &sources(&root));
        let outcome = run_reachability_bfs(
            &graph,
            &entry_plan(),
            &no_plugins(),
            &parse,
            &module_index,
            Vec::new(),
        );

        assert_eq!(outcome.reachable.len(), 4);
        for (file_id, line) in [(b_id, 2), (c_id, 3)] {
            let step = &outcome
                .predecessors
                .get(&file_id)
                .expect("predecessor")
                .step;
            assert!(
                matches!(step, TraceStep::DynamicImport { line: got, .. } if *got == line),
                "expected a dynamic import step, got {step:?}"
            );
        }

        assert_eq!(reached_from(&outcome, b_id), Some(main_id));
        assert_eq!(reached_from(&outcome, c_id), Some(b_id));
    }

    #[test]
    fn prefixed_dynamic_import_reaches_every_module_under_the_package() {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        };
        let modules = vec![ParsedModule {
            path: "src/acme/main.py".to_owned(),
            dynamic_import_prefixes: vec![DynamicImport {
                module: "acme".to_owned(),
                line: 4,
                ..DynamicImport::default()
            }],
            ..ParsedModule::default()
        }];
        let graph = graph_with_imports(root.clone(), &modules);
        let parse = ParseSummary { modules };

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        let module_index = ModuleIndex::build(&graph, &sources(&root));
        let outcome = run_reachability_bfs(
            &graph,
            &entry_plan(),
            &no_plugins(),
            &parse,
            &module_index,
            Vec::new(),
        );

        assert_eq!(outcome.reachable.len(), 4);
        for path in &PATHS[1..] {
            let file_id = graph.file_id(path).expect("file");
            let step = &outcome
                .predecessors
                .get(&file_id)
                .expect("predecessor")
                .step;
            assert!(
                matches!(step, TraceStep::DynamicImport { line: 4, .. }),
                "expected a dynamic import step, got {step:?}"
            );
            assert_eq!(reached_from(&outcome, file_id), Some(main_id));
        }
    }

    #[test]
    fn static_import_on_dynamic_import_line_stays_static() {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        };
        // `import acme.a; importlib.import_module("acme.a")` on one line.
        let modules = vec![parsed(
            "src/acme/main.py",
            &[("acme.a", 1)],
            &[("acme.a", 1)],
        )];
        let graph = graph_with_imports(root.clone(), &modules);
        let parse = ParseSummary { modules };

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        let a_id = graph.file_id("src/acme/a.py").expect("a");
        let module_index = ModuleIndex::build(&graph, &sources(&root));
        let outcome = run_reachability_bfs(
            &graph,
            &entry_plan(),
            &no_plugins(),
            &parse,
            &module_index,
            Vec::new(),
        );

        let step = &outcome.predecessors.get(&a_id).expect("predecessor").step;
        assert!(
            matches!(step, TraceStep::Import { line: 1, .. }),
            "expected a static import step, got {step:?}"
        );
        assert_eq!(reached_from(&outcome, a_id), Some(main_id));
    }

    #[test]
    fn from_package_import_submodule_reaches_the_submodule() {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        };
        let from_import = |name: &str, line: u32| ImportRef {
            module: "acme".to_owned(),
            name: Some(name.to_owned()),
            alias: None,
            line,
            kind: ImportKind::ImportFrom,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            deferred: false,
            relative_level: 0,
        };
        let modules = vec![ParsedModule {
            path: "src/acme/main.py".to_owned(),
            imports: vec![from_import("a", 1), from_import("not_a_module", 2)],
            ..ParsedModule::default()
        }];
        let graph = graph_with_imports(root.clone(), &modules);
        let parse = ParseSummary { modules };

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        let a_id = graph.file_id("src/acme/a.py").expect("a");
        let module_index = ModuleIndex::build(&graph, &sources(&root));
        let outcome = run_reachability_bfs(
            &graph,
            &entry_plan(),
            &no_plugins(),
            &parse,
            &module_index,
            Vec::new(),
        );

        assert_eq!(outcome.reachable.len(), 2);
        assert!(outcome.reachable.contains(&a_id));
        let step = &outcome.predecessors.get(&a_id).expect("predecessor").step;
        assert!(
            matches!(step, TraceStep::Import { module, line: 1 } if module == "acme.a"),
            "expected an import step via acme.a, got {step:?}"
        );
        assert_eq!(reached_from(&outcome, a_id), Some(main_id));
        assert_eq!(outcome.used_modules, []);
    }

    #[test]
    fn submodule_import_reaches_every_parent_package() {
        let root = test_root();
        let paths = [
            "src/acme/main.py",
            "src/acme/__init__.py",
            "src/acme/sub/__init__.py",
            "src/acme/sub/c.py",
        ];
        let modules = vec![parsed("src/acme/main.py", &[("acme.sub.c", 4)], &[])];
        let graph = graph_with_files(root.clone(), &paths, &["acme.sub.c"], &modules);
        let outcome = run_bfs(&graph, &root, modules, Vec::new());

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        assert_eq!(outcome.reachable.len(), 4);
        for (path, expected) in [
            ("src/acme/sub/c.py", "acme.sub.c"),
            ("src/acme/__init__.py", "acme"),
            ("src/acme/sub/__init__.py", "acme.sub"),
        ] {
            let file_id = graph.file_id(path).expect(path);
            let step = &outcome.predecessors.get(&file_id).expect(path).step;
            assert!(
                matches!(step, TraceStep::Import { module, line: 4 } if module == expected),
                "{path}: got {step:?}"
            );
            assert_eq!(reached_from(&outcome, file_id), Some(main_id));
        }
    }

    #[test]
    fn framework_seeds_are_followed_after_the_entry_walk() {
        let root = test_root();
        let modules = vec![
            parsed("src/acme/main.py", &[("acme.a", 1)], &[]),
            parsed("src/acme/c.py", &[("acme.b", 2)], &[]),
        ];
        let graph = graph_with_imports(root.clone(), &modules);
        let [a_id, b_id, c_id] = ["src/acme/a.py", "src/acme/b.py", "src/acme/c.py"]
            .map(|path| graph.file_id(path).expect(path));
        let seed = |file_id| {
            let step = TraceStep::PluginRef {
                module: "glob".to_owned(),
                label: "django".to_owned(),
            };
            (file_id, ReachPredecessor { from: None, step })
        };
        let outcome = run_bfs(&graph, &root, modules, vec![seed(a_id), seed(c_id)]);

        assert_eq!(outcome.reachable.len(), 4);
        let [a_step, b_from, c_step] = [a_id, b_id, c_id]
            .map(|file_id| outcome.predecessors.get(&file_id).expect("predecessor"));
        assert!(matches!(a_step.step, TraceStep::Import { line: 1, .. }));
        assert!(matches!(c_step.step, TraceStep::PluginRef { .. }));
        assert_eq!(b_from.from, Some(c_id));
    }

    #[test]
    fn eager_set_excludes_files_reached_only_through_imports_that_do_not_run() {
        let root = test_root();
        // a.py is imported inside a function, b.py under TYPE_CHECKING; c.py
        // is reached lazily through a.py and at module level from main.py.
        let mut main = parsed(
            "src/acme/main.py",
            &[("acme.a", 3), ("acme.b", 5), ("acme.c", 1)],
            &[],
        );
        main.imports[0].deferred = true;
        main.imports[1].context = ImportContext::Type;
        let modules = vec![main, parsed("src/acme/a.py", &[("acme.c", 1)], &[])];
        let graph = graph_with_imports(root.clone(), &modules);
        let outcome = run_bfs(&graph, &root, modules, Vec::new());

        let [main_id, _, _, c_id] = PATHS.map(|path| graph.file_id(path).expect(path));
        assert_eq!(outcome.reachable.len(), 4);
        assert_eq!(outcome.eager, IndexSet::from([main_id, c_id]));
    }

    /// #627: a file first reached lazily or optionally is followed again
    /// once a later path reaches it surely, so the files it imports are
    /// upgraded too, while its trace and its stdlib uses are kept from the
    /// first visit.
    #[test]
    fn upgraded_class_is_passed_on_to_files_already_reached() {
        let root = test_root();
        for weaken in [
            (|import: &mut ImportRef| import.deferred = true) as fn(&mut ImportRef),
            |import| import.optional = true,
        ] {
            let mut main = parsed("src/acme/main.py", &[("acme.a", 1), ("acme.b", 2)], &[]);
            weaken(&mut main.imports[0]);
            let modules = vec![
                main,
                parsed("src/acme/a.py", &[("acme.c", 1), ("os", 2)], &[]),
                parsed("src/acme/b.py", &[("acme.a", 1)], &[]),
            ];
            let mut graph = ProjectGraph::new(root.clone());
            graph.intern_module("os".to_owned(), ModuleOrigin::Stdlib);
            let graph = fill_graph(graph, &PATHS, &["acme.a", "acme.b", "acme.c"], &modules);
            let outcome = run_bfs(&graph, &root, modules, Vec::new());

            let [main_id, a_id, b_id, c_id] = PATHS.map(|path| graph.file_id(path).expect(path));
            let all = IndexSet::from([main_id, a_id, b_id, c_id]);
            assert_eq!(outcome.reachable, all);
            assert_eq!(outcome.eager, all);
            assert_eq!(outcome.certain, all);
            assert_eq!(reached_from(&outcome, a_id), Some(main_id));
            assert_eq!(reached_from(&outcome, c_id), Some(a_id));
            assert_eq!(outcome.used_modules.len(), 1);
        }
    }

    /// A file is queued again only when its class improves; reaching it
    /// again at the same or a weaker class leaves the queue alone.
    #[test]
    fn file_is_queued_again_only_when_its_class_improves() {
        let root = test_root();
        let graph = graph_with_imports(root.clone(), &[]);
        let module_index = ModuleIndex::build(&graph, &sources(&root));
        let entry = entry_plan();
        let adjacency = Adjacency {
            file_imports: HashMap::new(),
            submodule_imports: HashMap::new(),
            prefix_imports: HashMap::new(),
            dynamic_sites: HashSet::new(),
            lazy: HashSet::new(),
            optional: HashSet::new(),
        };
        let mut state = BfsState {
            graph: &graph,
            module_index: &module_index,
            entry: &entry,
            adjacency: &adjacency,
            queue: VecDeque::new(),
            reached: IndexMap::new(),
            predecessors: IndexMap::new(),
            used_modules: Vec::new(),
        };
        let file_id = graph.file_id(PATHS[1]).expect("a.py");
        let step = || TraceStep::File {
            file: file_id,
            path: PATHS[1].to_owned(),
        };

        state.enqueue_file(file_id, None, Reach::Eager, step);
        state.enqueue_file(file_id, None, Reach::Eager, step);
        state.enqueue_file(file_id, None, Reach::Reachable, step);
        assert_eq!(state.queue.len(), 1);
        state.enqueue_file(file_id, None, Reach::Certain, step);
        assert_eq!(state.queue.len(), 2);
        assert_eq!(state.reached[&file_id].class, Reach::Certain);
    }

    /// #614: a test importing a module says nothing about whether runtime
    /// code loads it, unless the module is a library's, which callers may
    /// import directly.
    #[test]
    fn eager_set_follows_test_imports_only_into_library_files() {
        let root = test_root();
        let modules = vec![parsed("src/acme/main.py", &[("acme.a", 1)], &[])];
        let mut graph = ProjectGraph::new(root.clone());
        for path in PATHS {
            let context = if path == "src/acme/main.py" {
                FileContext::Test
            } else {
                FileContext::Runtime
            };
            graph
                .intern_file(FileNode {
                    path: path.to_owned(),
                    context,
                    kind: FileKind::Python,
                })
                .expect("file");
        }
        graph.intern_module("acme.a".to_owned(), ModuleOrigin::FirstParty);
        let main_id = graph.file_id("src/acme/main.py").expect("main");
        add_parsed_imports(&mut graph, main_id, &modules[0]).expect("import edges");
        let a_id = graph.file_id("src/acme/a.py").expect("a");
        let parse = ParseSummary { modules };
        let module_index = ModuleIndex::build(&graph, &sources(&root));

        for (mode, eager) in [
            (ProjectMode::App, IndexSet::from([main_id])),
            (ProjectMode::Library, IndexSet::from([main_id, a_id])),
        ] {
            let entry = EntryPlan {
                mode,
                ..entry_plan()
            };
            let outcome = run_reachability_bfs(
                &graph,
                &entry,
                &no_plugins(),
                &parse,
                &module_index,
                Vec::new(),
            );
            assert_eq!(outcome.reachable, IndexSet::from([main_id, a_id]));
            assert_eq!(outcome.eager, eager, "{mode:?}");
        }
    }

    #[test]
    fn certain_set_excludes_files_reached_only_through_optional_imports() {
        let root = test_root();
        // a.py is imported under `try`/`suppress(ImportError)`, b.py under a
        // platform guard, and every file through an optional prefixed dynamic
        // import; c.py is reached optionally and from main.py too.
        let mut main = parsed(
            "src/acme/main.py",
            &[("acme.a", 3), ("acme.b", 5), ("acme.c", 1)],
            &[],
        );
        main.imports[0].optional = true;
        main.imports[1].platform_guarded = true;
        main.dynamic_import_prefixes.push(DynamicImport {
            module: "acme".to_owned(),
            line: 7,
            optional: true,
            ..DynamicImport::default()
        });
        let modules = vec![main, parsed("src/acme/a.py", &[("acme.c", 1)], &[])];
        let graph = graph_with_imports(root.clone(), &modules);
        let outcome = run_bfs(&graph, &root, modules, Vec::new());

        let [main_id, _, _, c_id] = PATHS.map(|path| graph.file_id(path).expect(path));
        assert_eq!(outcome.eager.len(), 4);
        assert_eq!(outcome.certain, IndexSet::from([main_id, c_id]));
    }
}
