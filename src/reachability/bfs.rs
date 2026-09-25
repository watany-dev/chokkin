//! Breadth-first reachability traversal.

use std::collections::{HashMap, HashSet, VecDeque};

use indexmap::{IndexMap, IndexSet};

use crate::entry::EntryPlan;
use crate::graph::{FileId, FileReachVia, GraphEdge, ModuleId, ModuleOrigin, ProjectGraph};
use crate::parser::ParseSummary;
use crate::plugins::PluginHints;
use crate::resolver::import_root;

use super::module_index::ModuleIndex;
use super::types::{ReachPredecessor, TraceStep, UsedModule};

/// Result of a BFS traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BfsOutcome {
    /// Files reached from entry roots, plugin refs, framework globs, and imports.
    pub reachable: IndexSet<FileId>,
    /// Shortest-path predecessors for trace reconstruction.
    pub predecessors: IndexMap<FileId, ReachPredecessor>,
    /// Stdlib and third-party modules encountered.
    pub used_modules: Vec<UsedModule>,
}

/// One import site on a file: the module it names and the line it sits on.
type ImportSite = (ModuleId, u32);

/// A `from pkg import name` site where `pkg.name` is itself a first-party
/// module: the resolved file, the dotted submodule name, and the line.
type SubmoduleSite = (FileId, String, u32);

/// An import site tagged with the file it appears in.
type ImportSiteRef = (FileId, ModuleId, u32);

struct BfsState<'a> {
    // Shared, so module and file names can be borrowed for the whole walk and
    // copied only into the steps of newly reached files.
    graph: &'a ProjectGraph,
    module_index: &'a ModuleIndex,
    file_imports: HashMap<FileId, Vec<ImportSite>>,
    submodule_imports: HashMap<FileId, Vec<SubmoduleSite>>,
    queue: VecDeque<FileId>,
    reachable: IndexSet<FileId>,
    predecessors: IndexMap<FileId, ReachPredecessor>,
    used_modules: Vec<UsedModule>,
    dynamic_sites: HashSet<ImportSiteRef>,
    /// `FileReachesFile` edges, one per (from, to), pushed once the walk ends.
    reach_edges: IndexMap<(FileId, FileId), FileReachVia>,
}

impl<'a> BfsState<'a> {
    fn new(graph: &'a ProjectGraph, parse: &ParseSummary, module_index: &'a ModuleIndex) -> Self {
        Self {
            graph,
            module_index,
            file_imports: build_file_import_adjacency(graph),
            submodule_imports: build_submodule_imports(graph, parse, module_index),
            queue: VecDeque::new(),
            reachable: IndexSet::new(),
            predecessors: IndexMap::new(),
            used_modules: Vec::new(),
            dynamic_sites: build_dynamic_sites(graph, parse),
            reach_edges: IndexMap::new(),
        }
    }

    /// Mark `file_id` reached; `step` is only built for a file seen the first time.
    fn enqueue_file(
        &mut self,
        file_id: FileId,
        from: Option<FileId>,
        step: impl FnOnce() -> TraceStep,
    ) {
        if self.reachable.insert(file_id) {
            self.predecessors
                .insert(file_id, ReachPredecessor { from, step: step() });
            self.queue.push_back(file_id);
        }
    }

    fn finish(self) -> (BfsOutcome, IndexMap<(FileId, FileId), FileReachVia>) {
        let outcome = BfsOutcome {
            reachable: self.reachable,
            predecessors: self.predecessors,
            used_modules: self.used_modules,
        };
        (outcome, self.reach_edges)
    }

    fn drain(&mut self) {
        while let Some(file_id) = self.queue.pop_front() {
            record_file_imports(self, file_id);
        }
    }
}

/// Run BFS from entry roots through first-party import edges.
///
/// Framework-glob files are seeded after the walk from the entry roots and
/// plugin refs has finished, so a file those reach keeps its import trace;
/// the imports of the seeded files are then followed like any other file's.
#[allow(clippy::too_many_arguments)]
pub fn run_reachability_bfs(
    graph: &mut ProjectGraph,
    entry: &EntryPlan,
    plugins: &PluginHints,
    parse: &ParseSummary,
    module_index: &ModuleIndex,
    framework: Vec<(FileId, ReachPredecessor)>,
) -> BfsOutcome {
    record_module_references(graph, plugins);
    let mut state = BfsState::new(graph, parse, module_index);

    for root in &entry.roots {
        let Some(file_id) = state.graph.file_id(&root.spec.path) else {
            continue;
        };
        state.enqueue_file(file_id, None, || TraceStep::File {
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
            state.enqueue_file(target, None, || TraceStep::PluginRef {
                module: name.to_owned(),
                label: label.to_owned(),
            });
        }
    }
    state.drain();

    for (file_id, ReachPredecessor { from, step }) in framework {
        state.enqueue_file(file_id, from, || step);
    }
    state.drain();

    let (outcome, reach_edges) = state.finish();
    for ((from, to), via) in reach_edges {
        graph.push_edge(GraphEdge::FileReachesFile { from, to, via });
    }
    outcome
}

fn record_module_references(graph: &mut ProjectGraph, plugins: &PluginHints) {
    for reference in plugins.module_refs() {
        let module = graph.module_id(&reference.module).unwrap_or_else(|| {
            graph.intern_module(reference.module.clone(), ModuleOrigin::Unknown)
        });
        graph.push_edge(GraphEdge::ConfigReferenceUsesModule {
            origin: reference.origin.clone(),
            module,
        });
    }
}

fn record_file_imports(state: &mut BfsState<'_>, file_id: FileId) {
    let graph = state.graph;
    // Taking the adjacency list out of the map avoids cloning it. Each file is
    // enqueued at most once, so it is never visited again after this.
    let imports = state.file_imports.remove(&file_id).unwrap_or_default();
    let source_path = graph.file(file_id).map_or("", |node| node.path.as_str());

    for (module_id, line) in imports {
        let Some(module_node) = graph.module(module_id) else {
            continue;
        };
        let name = module_node.name.as_str();
        match module_node.origin {
            ModuleOrigin::FirstParty => {
                let dynamic = state.dynamic_sites.contains(&(file_id, module_id, line));
                enqueue_import(state, file_id, name, line, dynamic);
            },
            origin @ (ModuleOrigin::Stdlib | ModuleOrigin::ThirdParty) => {
                state.used_modules.push(UsedModule {
                    full_module: name.to_owned(),
                    import_root: import_root(name).to_owned(),
                    origin,
                    file: source_path.to_owned(),
                    line,
                });
            },
            ModuleOrigin::Unknown => {},
        }
    }

    let submodules = state.submodule_imports.remove(&file_id).unwrap_or_default();
    for (target, module, line) in submodules {
        enqueue_resolved_module(state, target, file_id, FileReachVia::Import, || {
            TraceStep::Import { module, line }
        });
    }
}

/// Importing `pkg.sub.mod` runs `pkg/__init__.py` and `pkg/sub/__init__.py`
/// before `mod`, so every dotted prefix that resolves is reached from the
/// same site.
fn enqueue_import(
    state: &mut BfsState<'_>,
    from_file: FileId,
    module: &str,
    line: u32,
    dynamic: bool,
) {
    let via = if dynamic {
        FileReachVia::DynamicImport
    } else {
        FileReachVia::Import
    };
    for name in module_and_parents(module) {
        let Some(target) = state.module_index.resolve(name) else {
            continue;
        };
        enqueue_resolved_module(state, target, from_file, via, || {
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

fn enqueue_resolved_module(
    state: &mut BfsState<'_>,
    target: FileId,
    from_file: FileId,
    via: FileReachVia,
    step: impl FnOnce() -> TraceStep,
) {
    // The first site that links a pair decides its `via`.
    if from_file != target {
        state.reach_edges.entry((from_file, target)).or_insert(via);
    }
    state.enqueue_file(target, Some(from_file), step);
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
            if let Some(target) = module_index.resolve(&submodule) {
                sites
                    .entry(file_id)
                    .or_default()
                    .push((target, submodule, import.line));
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
        for dynamic in &module.dynamic_imports {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EntrySpec, ProjectMode};
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::entry::{EntryRoot, ResolvedMode};
    use crate::graph::{FileNode, add_parsed_imports};
    use crate::parser::{DynamicImport, ImportContext, ImportKind, ImportRef, ParsedModule};
    use crate::resolver::ResolveConfidence;
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
                    relative_level: 0,
                })
                .collect(),
            dynamic_imports: dynamic
                .iter()
                .map(|(module, line)| DynamicImport {
                    module: (*module).to_owned(),
                    line: *line,
                })
                .collect(),
            ..ParsedModule::default()
        }
    }

    fn layout() -> LayoutInfo {
        LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
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
            mode: ResolvedMode {
                mode: ProjectMode::App,
                confidence: ResolveConfidence::Certain,
            },
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
        let mut graph = ProjectGraph::new(root);
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
            start: std::env::temp_dir(),
        }
    }

    fn run_bfs(
        graph: &mut ProjectGraph,
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

    fn reach_edges(graph: &ProjectGraph) -> Vec<(FileId, FileId, FileReachVia)> {
        graph
            .edges()
            .iter()
            .filter_map(|edge| match edge {
                GraphEdge::FileReachesFile { from, to, via } => Some((*from, *to, *via)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn dynamic_import_reach_is_recorded_once_and_kept_dynamic() {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        };
        let modules = vec![
            parsed("src/acme/main.py", &[("acme.a", 1)], &[("acme.b", 2)]),
            parsed("src/acme/b.py", &[], &[("acme.c", 3)]),
        ];
        let mut graph = graph_with_imports(root.clone(), &modules);
        let parse = ParseSummary { modules };

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        let b_id = graph.file_id("src/acme/b.py").expect("b");
        let c_id = graph.file_id("src/acme/c.py").expect("c");
        let module_index = ModuleIndex::build(&graph, &sources(&root));
        let outcome = run_reachability_bfs(
            &mut graph,
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

        let edges = reach_edges(&graph);
        assert_eq!(edges.len(), 3);
        assert!(edges.contains(&(main_id, b_id, FileReachVia::DynamicImport)));
        assert!(edges.contains(&(b_id, c_id, FileReachVia::DynamicImport)));
    }

    #[test]
    fn static_import_on_dynamic_import_line_stays_static() {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        };
        // `import acme.a; importlib.import_module("acme.a")` on one line.
        let modules = vec![parsed(
            "src/acme/main.py",
            &[("acme.a", 1)],
            &[("acme.a", 1)],
        )];
        let mut graph = graph_with_imports(root.clone(), &modules);
        let parse = ParseSummary { modules };

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        let a_id = graph.file_id("src/acme/a.py").expect("a");
        let module_index = ModuleIndex::build(&graph, &sources(&root));
        let outcome = run_reachability_bfs(
            &mut graph,
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
        assert_eq!(
            reach_edges(&graph),
            vec![(main_id, a_id, FileReachVia::Import)]
        );
    }

    #[test]
    fn from_package_import_submodule_reaches_the_submodule() {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
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
            relative_level: 0,
        };
        let modules = vec![ParsedModule {
            path: "src/acme/main.py".to_owned(),
            imports: vec![from_import("a", 1), from_import("not_a_module", 2)],
            ..ParsedModule::default()
        }];
        let mut graph = graph_with_imports(root.clone(), &modules);
        let parse = ParseSummary { modules };

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        let a_id = graph.file_id("src/acme/a.py").expect("a");
        let module_index = ModuleIndex::build(&graph, &sources(&root));
        let outcome = run_reachability_bfs(
            &mut graph,
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
        assert_eq!(
            reach_edges(&graph),
            vec![(main_id, a_id, FileReachVia::Import)]
        );
        assert!(outcome.used_modules.is_empty());
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
        let mut graph = graph_with_files(root.clone(), &paths, &["acme.sub.c"], &modules);
        let outcome = run_bfs(&mut graph, &root, modules, Vec::new());

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        let edges = reach_edges(&graph);
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
            assert!(edges.contains(&(main_id, file_id, FileReachVia::Import)));
        }
    }

    #[test]
    fn framework_seeds_are_followed_after_the_entry_walk() {
        let root = test_root();
        let modules = vec![
            parsed("src/acme/main.py", &[("acme.a", 1)], &[]),
            parsed("src/acme/c.py", &[("acme.b", 2)], &[]),
        ];
        let mut graph = graph_with_imports(root.clone(), &modules);
        let [a_id, b_id, c_id] = ["src/acme/a.py", "src/acme/b.py", "src/acme/c.py"]
            .map(|path| graph.file_id(path).expect(path));
        let seed = |file_id| {
            let step = TraceStep::PluginRef {
                module: "glob".to_owned(),
                label: "django".to_owned(),
            };
            (file_id, ReachPredecessor { from: None, step })
        };
        let outcome = run_bfs(&mut graph, &root, modules, vec![seed(a_id), seed(c_id)]);

        assert_eq!(outcome.reachable.len(), 4);
        let [a_step, b_from, c_step] = [a_id, b_id, c_id]
            .map(|file_id| outcome.predecessors.get(&file_id).expect("predecessor"));
        assert!(matches!(a_step.step, TraceStep::Import { line: 1, .. }));
        assert!(matches!(c_step.step, TraceStep::PluginRef { .. }));
        assert_eq!(b_from.from, Some(c_id));
        assert!(reach_edges(&graph).contains(&(c_id, b_id, FileReachVia::Import)));
    }
}
