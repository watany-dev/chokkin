//! Breadth-first reachability traversal.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::entry::EntryPlan;
use crate::graph::{FileId, FileReachVia, GraphEdge, ModuleId, ModuleOrigin, ProjectGraph};
use crate::parser::ParseSummary;
use crate::plugins::{PluginHints, ReferenceOrigin};
use crate::resolver::import_root;

use super::module_index::ModuleIndex;
use super::types::{ReachPredecessor, TraceStep, UsedModule};

/// Result of a BFS traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BfsOutcome {
    /// Files reached from entry roots, plugin refs, and imports.
    pub reachable: HashSet<FileId>,
    /// Shortest-path predecessors for trace reconstruction.
    pub predecessors: indexmap::IndexMap<FileId, ReachPredecessor>,
    /// Stdlib and third-party modules encountered.
    pub used_modules: Vec<UsedModule>,
}

/// One import site on a file: the module it names and the line it sits on.
type ImportSite = (ModuleId, u32);

/// A `from pkg import name` site where `pkg.name` is itself a first-party
/// module: the resolved file, the dotted submodule name, and the line.
type SubmoduleSite = (FileId, String, u32);

struct BfsState<'a> {
    graph: &'a mut ProjectGraph,
    module_index: &'a ModuleIndex,
    file_imports: HashMap<FileId, Vec<ImportSite>>,
    submodule_imports: HashMap<FileId, Vec<SubmoduleSite>>,
    queue: VecDeque<FileId>,
    reachable: HashSet<FileId>,
    predecessors: indexmap::IndexMap<FileId, ReachPredecessor>,
    used_modules: Vec<UsedModule>,
    dynamic_sites: HashSet<ImportSiteRef>,
    reach_edges: HashSet<(FileId, FileId)>,
}

/// An import site tagged with the file it appears in.
type ImportSiteRef = (FileId, ModuleId, u32);

impl<'a> BfsState<'a> {
    fn new(
        graph: &'a mut ProjectGraph,
        module_index: &'a ModuleIndex,
        file_imports: HashMap<FileId, Vec<ImportSite>>,
        submodule_imports: HashMap<FileId, Vec<SubmoduleSite>>,
        dynamic_sites: HashSet<ImportSiteRef>,
    ) -> Self {
        Self {
            graph,
            module_index,
            file_imports,
            submodule_imports,
            queue: VecDeque::new(),
            reachable: HashSet::new(),
            predecessors: indexmap::IndexMap::new(),
            used_modules: Vec::new(),
            dynamic_sites,
            reach_edges: HashSet::new(),
        }
    }

    fn finish(self) -> BfsOutcome {
        BfsOutcome {
            reachable: self.reachable,
            predecessors: self.predecessors,
            used_modules: self.used_modules,
        }
    }

    fn enqueue_file(&mut self, file_id: FileId, from: Option<FileId>, step: TraceStep) {
        if self.reachable.contains(&file_id) {
            return;
        }
        self.reachable.insert(file_id);
        self.predecessors
            .insert(file_id, ReachPredecessor { from, step });
        self.queue.push_back(file_id);
    }
}

/// Run BFS from entry roots through first-party import edges.
pub fn run_reachability_bfs(
    graph: &mut ProjectGraph,
    entry: &EntryPlan,
    plugins: &PluginHints,
    parse: &ParseSummary,
    module_index: &ModuleIndex,
) -> BfsOutcome {
    let file_imports = build_file_import_adjacency(graph);
    let submodule_imports = build_submodule_imports(graph, parse, module_index);
    let dynamic_sites = build_dynamic_sites(graph, parse);
    let mut state = BfsState::new(
        graph,
        module_index,
        file_imports,
        submodule_imports,
        dynamic_sites,
    );

    for root in &entry.roots {
        let Some(file_id) = state.graph.file_id(&root.spec.path) else {
            continue;
        };
        state.enqueue_file(
            file_id,
            None,
            TraceStep::File {
                file: file_id,
                path: root.spec.path.clone(),
            },
        );
    }

    for reference in plugins.module_refs() {
        enqueue_module_reference(&mut state, &reference.module, &reference.origin);
    }

    while let Some(file_id) = state.queue.pop_front() {
        record_file_imports(&mut state, file_id);
    }

    state.finish()
}

fn record_file_imports(state: &mut BfsState<'_>, file_id: FileId) {
    // Taking the adjacency list out of the map avoids cloning it. Each file is
    // enqueued at most once, so it is never visited again after this.
    let imports = state.file_imports.remove(&file_id).unwrap_or_default();
    // Only third-party and stdlib imports name the source file, so projects
    // whose imports are mostly first-party never build this string.
    let mut source_path: Option<String> = None;

    for (module_id, line) in imports {
        let dynamic = state.dynamic_sites.contains(&(file_id, module_id, line));
        let Some(module_node) = state.graph.module(module_id) else {
            continue;
        };
        let module_name = module_node.name.clone();
        let module_origin = module_node.origin;
        match module_origin {
            ModuleOrigin::FirstParty => {
                // Resolving before the step is built lets the step own the
                // module name instead of taking a second copy of it.
                let Some(target) = state.module_index.resolve(&module_name) else {
                    continue;
                };
                let (step, via) = if dynamic {
                    (
                        TraceStep::DynamicImport {
                            module: module_name,
                            line,
                        },
                        FileReachVia::DynamicImport,
                    )
                } else {
                    (
                        TraceStep::Import {
                            module: module_name,
                            line,
                        },
                        FileReachVia::Import,
                    )
                };
                enqueue_resolved_module(state, target, file_id, step, via);
            },
            ModuleOrigin::Stdlib | ModuleOrigin::ThirdParty => {
                let import_root = import_root(&module_name).to_owned();
                let file = source_path
                    .get_or_insert_with(|| {
                        state
                            .graph
                            .file(file_id)
                            .map_or_else(String::new, |node| node.path.clone())
                    })
                    .clone();
                state.used_modules.push(UsedModule {
                    full_module: module_name,
                    import_root,
                    origin: module_origin,
                    file,
                    line,
                });
            },
            ModuleOrigin::Unknown => {},
        }
    }

    let submodules = state.submodule_imports.remove(&file_id).unwrap_or_default();
    for (target, module, line) in submodules {
        enqueue_resolved_module(
            state,
            target,
            file_id,
            TraceStep::Import { module, line },
            FileReachVia::Import,
        );
    }
}

fn enqueue_resolved_module(
    state: &mut BfsState<'_>,
    target: FileId,
    from_file: FileId,
    step: TraceStep,
    via: FileReachVia,
) {
    // One edge per (from, to): the same pair is otherwise pushed again for
    // every further import site that resolves to the target file.
    if from_file != target && state.reach_edges.insert((from_file, target)) {
        state.graph.push_edge(GraphEdge::FileReachesFile {
            from: from_file,
            to: target,
            via,
        });
    }
    state.enqueue_file(target, Some(from_file), step);
}

fn enqueue_module_reference(state: &mut BfsState<'_>, module: &str, origin: &ReferenceOrigin) {
    let module_id = state
        .graph
        .intern_module(module.to_owned(), ModuleOrigin::Unknown);
    state.graph.push_edge(GraphEdge::ConfigReferenceUsesModule {
        origin: origin.clone(),
        module: module_id,
    });
    let Some(target) = state.module_index.resolve(module) else {
        return;
    };
    state.enqueue_file(
        target,
        None,
        TraceStep::PluginRef {
            module: module.to_owned(),
            label: origin.label.clone(),
        },
    );
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
/// parse summary is what distinguishes them.
fn build_dynamic_sites(graph: &ProjectGraph, parse: &ParseSummary) -> HashSet<ImportSiteRef> {
    let mut sites = HashSet::new();
    for module in &parse.modules {
        let Some(file_id) = graph.file_id(&module.path) else {
            continue;
        };
        for dynamic in &module.dynamic_imports {
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
            warnings: Vec::new(),
        }
    }

    fn graph_with_imports(root: ProjectRoot, modules: &[ParsedModule]) -> ProjectGraph {
        let mut graph = ProjectGraph::new(root);
        for path in PATHS {
            graph
                .intern_file(FileNode {
                    path: path.to_owned(),
                    context: FileContext::Runtime,
                    kind: FileKind::Python,
                })
                .expect("file");
        }
        for module in ["acme.a", "acme.b", "acme.c"] {
            graph.intern_module(module.to_owned(), ModuleOrigin::FirstParty);
        }
        for parsed in modules {
            let file_id = graph.file_id(&parsed.path).expect("parsed file");
            add_parsed_imports(&mut graph, file_id, parsed).expect("import edges");
        }
        graph
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
}
