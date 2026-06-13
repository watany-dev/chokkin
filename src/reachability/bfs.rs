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

struct BfsState<'a> {
    graph: &'a mut ProjectGraph,
    module_index: &'a ModuleIndex,
    queue: VecDeque<FileId>,
    reachable: HashSet<FileId>,
    predecessors: indexmap::IndexMap<FileId, ReachPredecessor>,
    used_modules: Vec<UsedModule>,
}

impl<'a> BfsState<'a> {
    fn new(graph: &'a mut ProjectGraph, module_index: &'a ModuleIndex) -> Self {
        Self {
            graph,
            module_index,
            queue: VecDeque::new(),
            reachable: HashSet::new(),
            predecessors: indexmap::IndexMap::new(),
            used_modules: Vec::new(),
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
    let mut state = BfsState::new(graph, module_index);
    let parse_by_path = parse
        .modules
        .iter()
        .map(|module| (module.path.as_str(), module))
        .collect::<HashMap<_, _>>();

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

        if let Some(parsed) = state
            .graph
            .file(file_id)
            .and_then(|node| parse_by_path.get(node.path.as_str()).copied())
        {
            for dynamic in &parsed.dynamic_imports {
                enqueue_resolved_module(
                    &mut state,
                    &dynamic.module,
                    file_id,
                    TraceStep::DynamicImport {
                        module: dynamic.module.clone(),
                        line: dynamic.line,
                    },
                    FileReachVia::DynamicImport,
                );
            }
        }
    }

    state.finish()
}

fn record_file_imports(state: &mut BfsState<'_>, file_id: FileId) {
    let imports = file_import_edges(state.graph, file_id);
    let source_path = state
        .graph
        .file(file_id)
        .map_or_else(String::new, |node| node.path.clone());

    for (module_id, line) in imports {
        let Some(module_node) = state.graph.module(module_id) else {
            continue;
        };
        let module_name = module_node.name.clone();
        let module_origin = module_node.origin;
        match module_origin {
            ModuleOrigin::FirstParty => {
                enqueue_resolved_module(
                    state,
                    &module_name,
                    file_id,
                    TraceStep::Import {
                        module: module_name.clone(),
                        line,
                    },
                    FileReachVia::Import,
                );
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
    if from_file != target {
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

fn file_import_edges(graph: &ProjectGraph, file_id: FileId) -> Vec<(ModuleId, u32)> {
    graph
        .edges()
        .iter()
        .filter_map(|edge| match edge {
            GraphEdge::FileImportsModule { file, module, line } if *file == file_id => {
                Some((*module, *line))
            },
            _ => None,
        })
        .collect()
}
