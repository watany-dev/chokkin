//! Breadth-first reachability traversal.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::entry::EntryPlan;
use crate::graph::{FileId, FileReachVia, GraphEdge, ModuleId, ModuleOrigin, ProjectGraph};
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

/// One import site on a file, in graph edge order (static imports first).
type ImportSite = (ModuleId, u32, bool);

struct BfsState<'a> {
    graph: &'a mut ProjectGraph,
    module_index: &'a ModuleIndex,
    file_imports: HashMap<FileId, Vec<ImportSite>>,
    queue: VecDeque<FileId>,
    reachable: HashSet<FileId>,
    predecessors: indexmap::IndexMap<FileId, ReachPredecessor>,
    used_modules: Vec<UsedModule>,
    reach_edges: HashSet<(FileId, FileId)>,
}

impl<'a> BfsState<'a> {
    fn new(
        graph: &'a mut ProjectGraph,
        module_index: &'a ModuleIndex,
        file_imports: HashMap<FileId, Vec<ImportSite>>,
    ) -> Self {
        Self {
            graph,
            module_index,
            file_imports,
            queue: VecDeque::new(),
            reachable: HashSet::new(),
            predecessors: indexmap::IndexMap::new(),
            used_modules: Vec::new(),
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
    module_index: &ModuleIndex,
) -> BfsOutcome {
    let file_imports = build_file_import_adjacency(graph);
    let mut state = BfsState::new(graph, module_index, file_imports);

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
    let imports = state
        .file_imports
        .get(&file_id)
        .cloned()
        .unwrap_or_default();
    let source_path = state
        .graph
        .file(file_id)
        .map_or_else(String::new, |node| node.path.clone());

    for (module_id, line, dynamic) in imports {
        let Some(module_node) = state.graph.module(module_id) else {
            continue;
        };
        let module_name = module_node.name.clone();
        let module_origin = module_node.origin;
        match module_origin {
            ModuleOrigin::FirstParty => {
                let (step, via) = if dynamic {
                    (
                        TraceStep::DynamicImport {
                            module: module_name.clone(),
                            line,
                        },
                        FileReachVia::DynamicImport,
                    )
                } else {
                    (
                        TraceStep::Import {
                            module: module_name.clone(),
                            line,
                        },
                        FileReachVia::Import,
                    )
                };
                enqueue_resolved_module(state, &module_name, file_id, step, via);
            },
            ModuleOrigin::Stdlib | ModuleOrigin::ThirdParty => {
                let import_root = import_root(&module_name).to_owned();
                state.used_modules.push(UsedModule {
                    full_module: module_name,
                    import_root,
                    origin: module_origin,
                    file: source_path.clone(),
                    line,
                });
            },
            ModuleOrigin::Unknown => {},
        }
    }
}

fn enqueue_resolved_module(
    state: &mut BfsState<'_>,
    module: &str,
    from_file: FileId,
    step: TraceStep,
    via: FileReachVia,
) {
    let Some(target) = state.module_index.resolve(module) else {
        return;
    };
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
        if let GraphEdge::FileImportsModule {
            file,
            module,
            line,
            dynamic,
        } = edge
        {
            adjacency
                .entry(*file)
                .or_default()
                .push((*module, *line, *dynamic));
        }
    }
    adjacency
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
            attribute_accesses: Vec::new(),
            symbols: Vec::new(),
            exports: Vec::new(),
            ignores: Vec::new(),
            has_opaque_dynamic_import: false,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn dynamic_import_reach_is_recorded_once_and_kept_dynamic() {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        };
        let paths = [
            "src/acme/main.py",
            "src/acme/a.py",
            "src/acme/b.py",
            "src/acme/c.py",
        ];
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
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
        };

        let mut graph = ProjectGraph::new(root);
        for path in paths {
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

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        let b_id = graph.file_id("src/acme/b.py").expect("b");
        let c_id = graph.file_id("src/acme/c.py").expect("c");
        add_parsed_imports(
            &mut graph,
            main_id,
            &parsed("src/acme/main.py", &[("acme.a", 1)], &[("acme.b", 2)]),
        )
        .expect("main edges");
        add_parsed_imports(
            &mut graph,
            b_id,
            &parsed("src/acme/b.py", &[], &[("acme.c", 3)]),
        )
        .expect("b edges");

        let entry = EntryPlan {
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
        };
        let plugins = PluginHints {
            contributions: Vec::new(),
            config_binary_usages: Vec::new(),
            config_used_distributions: Vec::new(),
            warnings: Vec::new(),
        };
        let module_index = ModuleIndex::build(&graph, &sources);
        let outcome = run_reachability_bfs(&mut graph, &entry, &plugins, &module_index);

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

        let reach_edges: Vec<_> = graph
            .edges()
            .iter()
            .filter_map(|edge| match edge {
                GraphEdge::FileReachesFile { from, to, via } => Some((*from, *to, *via)),
                _ => None,
            })
            .collect();
        assert_eq!(reach_edges.len(), 3);
        assert!(reach_edges.contains(&(main_id, b_id, FileReachVia::DynamicImport)));
        assert!(reach_edges.contains(&(b_id, c_id, FileReachVia::DynamicImport)));
    }
}
