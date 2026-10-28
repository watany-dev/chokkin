//! Breadth-first reachability traversal.

use std::collections::{HashMap, HashSet, VecDeque};

use indexmap::{IndexMap, IndexSet};

use crate::config::ProjectMode;
use crate::entry::EntryPlan;
use crate::graph::{FileId, GraphEdge, ModuleId, ModuleOrigin, ProjectGraph};
use crate::parser::{ImportContext, ParseSummary};
use crate::path_util::is_submodule;
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
    /// (`try`/`suppress(ImportError)` or platform-guarded) import either,
    /// counting paths that start at a library's public surface.
    pub exposed: IndexSet<FileId>,
    /// Exposed files that some path reaches from a root the package or its
    /// host loads, not only from public subpackages or tests (#802).
    pub certain: IndexSet<FileId>,
    /// Paths of the files a reached file names in a list of module names
    /// (#1257): the modules a loader imports by name, not the packages above
    /// them.
    pub listed: HashSet<String>,
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
    /// Reached through imports that always run, but only from a library
    /// subpackage's public surface or out of a test, docs or dev file:
    /// callers may import it, but nothing shows the package does (#802).
    Exposed,
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
    listed_imports: HashMap<FileId, Vec<SubmoduleSite>>,
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
    listed: IndexSet<FileId>,
}

impl BfsState<'_> {
    /// Imports out of test, docs and dev files only count for library files,
    /// which outside callers may import directly, and only as far as
    /// [`Reach::Exposed`] into a submodule the package need not load.
    fn edge_class(&self, from: FileId, mut class: Reach, line: u32, target: FileId) -> Reach {
        if class == Reach::Reachable || self.adjacency.lazy.contains(&(from, line)) {
            return Reach::Reachable;
        }
        let file = |id| self.graph.file(id);
        if file(from).is_some_and(|node| node.context != FileContext::Runtime) {
            let Some(target) =
                file(target).filter(|node| self.entry.mode_for(&node.path) == ProjectMode::Library)
            else {
                return Reach::Reachable;
            };
            let runtime_init = |init: &str| {
                self.graph
                    .file_id(init)
                    .and_then(file)
                    .is_some_and(|node| node.context == FileContext::Runtime)
            };
            if is_submodule(&target.path, runtime_init) {
                class = class.min(Reach::Exposed);
            }
        }
        if self.adjacency.optional.contains(&(from, line)) {
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
        let exposed = at_least(Reach::Exposed);
        let certain = at_least(Reach::Certain);
        BfsOutcome {
            reachable: self.reached.keys().copied().collect(),
            predecessors: self.predecessors,
            used_modules: self.used_modules,
            eager,
            exposed,
            certain,
            listed: self
                .listed
                .iter()
                .filter_map(|file| Some(self.graph.file(*file)?.path.clone()))
                .collect(),
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
/// so no file is followed more than four times.
///
/// Framework-glob files are seeded after the walk from the entry roots and
/// plugin refs has finished, so a file those reach keeps its import trace;
/// the imports of the seeded files are then followed like any other file's.
#[expect(
    clippy::too_many_arguments,
    reason = "takes one output per earlier pipeline phase; a bundle struct would only rename them"
)]
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
        listed_imports: build_listed_imports(graph, parse, module_index),
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
        listed: IndexSet::new(),
    };

    for root in &entry.roots {
        let Some(file_id) = state.graph.file_id(&root.path) else {
            continue;
        };
        let class = if root.only_public() {
            Reach::Exposed
        } else {
            Reach::Certain
        };
        state.enqueue_file(file_id, None, class, || TraceStep::File {
            file: file_id,
            path: root.path.clone(),
        });
    }
    for reference in plugins.module_refs() {
        let label = reference.origin.label.as_str();
        // A library module's own decorators register it only once some
        // caller imports it, like a public subpackage (#802).
        let origin = reference.origin.file.as_str();
        let self_registered = state.graph.file_id(origin).is_some_and(|file| {
            state.module_index.resolve_from(origin, &reference.module) == Some(file)
        });
        let class = if self_registered && entry.mode_for(origin) == ProjectMode::Library {
            Reach::Exposed
        } else {
            Reach::Certain
        };
        for name in module_and_parents(&reference.module) {
            let Some(target) = state
                .module_index
                .resolve_from(&reference.origin.file, name)
            else {
                continue;
            };
            state.enqueue_file(target, None, class, || TraceStep::PluginRef {
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

    // Nothing shows when the loader runs, so a listed module loads lazily.
    for (listed, module, line) in adjacency.listed_imports.get(&file_id).into_iter().flatten() {
        state.listed.insert(*listed);
        for name in module_and_parents(module) {
            let Some(target) = state.module_index.resolve_from(source_path, name) else {
                continue;
            };
            state.enqueue_file(target, Some(file_id), Reach::Reachable, || {
                TraceStep::DynamicImport {
                    module: name.to_owned(),
                    line: *line,
                }
            });
        }
    }
}

/// Importing `pkg.sub.mod` runs `pkg/__init__.py` and `pkg/sub/__init__.py`
/// before `mod`, so every dotted prefix that resolves is reached from the
/// same site.
#[expect(
    clippy::too_many_arguments,
    reason = "one import site's fields plus the BFS state they are queued into"
)]
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
pub(crate) fn module_and_parents(module: &str) -> impl Iterator<Item = &str> {
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
    // Most of these lookups miss (`from pkg import Class`), and a miss tries
    // every fallback: resolve per module on workers, merge in module order.
    let per_module = crate::parallel::map_in_order(&parse.modules, |module| {
        let file_id = graph.file_id(&module.path)?;
        let mut found = Vec::new();
        for import in &module.imports {
            let Some(name) = import.name.as_deref() else {
                continue;
            };
            if import.module.is_empty() {
                continue;
            }
            let submodule = format!("{}.{name}", import.module);
            if let Some(target) = module_index.resolve_from(&module.path, &submodule) {
                found.push((target, submodule, import.line));
            }
        }
        (!found.is_empty()).then_some((file_id, found))
    });
    let mut sites: HashMap<FileId, Vec<SubmoduleSite>> = HashMap::new();
    for (file_id, found) in per_module.into_iter().flatten() {
        sites.entry(file_id).or_default().extend(found);
    }
    sites
}

/// `import_module("pkg.commands." + name)` may load any module under
/// `pkg.commands`, so each such site reaches all of them, and `pkg` and
/// `pkg.commands`, which Python imports first whatever `name` is (#1108).
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
            let packages = module_and_parents(&prefix.module).filter_map(|name| {
                module_index
                    .resolve_from(&module.path, name)
                    .map(|target| (name, target))
            });
            for (name, target) in packages.chain(module_index.under(&prefix.module)) {
                sites
                    .entry(file_id)
                    .or_default()
                    .push((target, name.to_owned(), prefix.line));
            }
        }
    }
    sites
}

/// A list of dotted module names in a runtime file is a plugin list some
/// loader imports one by one (redash's `default_query_runners`, #1257), so
/// each site reaches the module and, when the walk follows it, the packages
/// Python imports before it. Only a list whose every name resolves to a project module counts: one
/// that also names a third-party module, a class or a typo may be anything
/// (a log message's names, `MIDDLEWARE`), so it reaches nothing and lowers
/// nothing, and its strings stay [`crate::parser::ModuleRef`]s.
fn build_listed_imports(
    graph: &ProjectGraph,
    parse: &ParseSummary,
    module_index: &ModuleIndex,
) -> HashMap<FileId, Vec<SubmoduleSite>> {
    let mut sites: HashMap<FileId, Vec<SubmoduleSite>> = HashMap::new();
    for module in &parse.modules {
        let Some(file_id) = graph.file_id(&module.path).filter(|id| {
            graph
                .file(*id)
                .is_some_and(|node| node.context == FileContext::Runtime)
        }) else {
            continue;
        };
        for list in &module.module_lists {
            let Some(targets) = list
                .iter()
                .map(|reference| module_index.resolve_from(&module.path, &reference.text))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            for (reference, target) in list.iter().zip(targets) {
                sites.entry(file_id).or_default().push((
                    target,
                    reference.text.clone(),
                    reference.line,
                ));
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
    use crate::config::{PluginId, ProjectMode};
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::entry::{EntryOrigin, EntryRoot, LIBRARY_SUBPACKAGE_RULE};
    use crate::graph::{FileNode, add_parsed_imports};
    use crate::parser::{DynamicImport, ImportContext, ImportKind, ImportRef, ParsedModule};
    use crate::plugins::{ModuleReference, PluginContribution, ReferenceOrigin};
    use crate::sources::{DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo};
    use std::collections::BTreeSet;

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
                    fallback_for: Vec::new(),
                    version_minors: None,
                    called_before: Vec::new(),
                    tested_before: Vec::new(),
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
        LayoutInfo::new("src", vec!["acme".to_owned()])
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
            distributed_members: Vec::new(),
            roots: vec![EntryRoot {
                path: "src/acme/main.py".to_owned(),
                symbols: BTreeSet::new(),
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
            config_requirement_names: Vec::new(),
            unread_requirements: Vec::new(),
            config_module_refs: Vec::new(),
            config_used_symbols: Vec::new(),
            config_script_entries: Vec::new(),
            config_command_targets: Vec::new(),
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

    /// `import_module("acme.plugins." + name)` imports `acme` and
    /// `acme.plugins` before any module under it (#1108).
    #[test]
    fn prefixed_dynamic_import_reaches_the_package_and_its_parents() {
        const FILES: [&str; 4] = [
            "src/acme/__init__.py",
            "src/acme/main.py",
            "src/acme/plugins/__init__.py",
            "src/acme/plugins/one.py",
        ];
        let root = test_root();
        let mut main = parsed("src/acme/main.py", &[], &[]);
        main.dynamic_import_prefixes.push(DynamicImport {
            module: "acme.plugins".to_owned(),
            line: 2,
            ..DynamicImport::default()
        });
        let modules = vec![main];
        let graph = graph_with_files(root.clone(), &FILES, &[], &modules);
        let sources = DiscoveredSources {
            files: FILES
                .iter()
                .map(|path| DiscoveredFile {
                    path: (*path).to_owned(),
                    kind: FileKind::Python,
                    context: FileContext::Runtime,
                })
                .collect(),
            ..sources(&root)
        };
        let module_index = ModuleIndex::build(&graph, &sources);
        let outcome = run_reachability_bfs(
            &graph,
            &entry_plan(),
            &no_plugins(),
            &ParseSummary { modules },
            &module_index,
            Vec::new(),
        );

        let main_id = graph.file_id("src/acme/main.py").expect("main");
        for path in [FILES[0], FILES[2], FILES[3]] {
            let file_id = graph.file_id(path).expect(path);
            assert!(outcome.certain.contains(&file_id), "{path}");
            assert_eq!(reached_from(&outcome, file_id), Some(main_id), "{path}");
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
    fn static_import_of_another_module_on_the_line_keeps_it_dynamic() {
        let root = ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        };
        // `import acme.a; importlib.import_module("acme.b")` on one line.
        let modules = vec![parsed(
            "src/acme/main.py",
            &[("acme.a", 1)],
            &[("acme.b", 1)],
        )];
        let graph = graph_with_imports(root.clone(), &modules);
        let parse = ParseSummary { modules };

        let b_id = graph.file_id("src/acme/b.py").expect("b");
        let module_index = ModuleIndex::build(&graph, &sources(&root));
        let outcome = run_reachability_bfs(
            &graph,
            &entry_plan(),
            &no_plugins(),
            &parse,
            &module_index,
            Vec::new(),
        );

        let step = &outcome.predecessors.get(&b_id).expect("predecessor").step;
        assert!(
            matches!(step, TraceStep::DynamicImport { line: 1, .. }),
            "expected a dynamic import step, got {step:?}"
        );
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
            fallback_for: Vec::new(),
            version_minors: None,
            called_before: Vec::new(),
            tested_before: Vec::new(),
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
            listed_imports: HashMap::new(),
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
            listed: IndexSet::new(),
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
    /// import directly. #802: past a package `__init__.py` that need not
    /// load it, the module is exposed but not certain.
    #[test]
    fn eager_set_follows_test_imports_only_into_library_files() {
        for with_init in [false, true] {
            check_test_import_into_library(with_init);
        }
    }

    fn check_test_import_into_library(with_init: bool) {
        let root = test_root();
        let modules = vec![parsed("src/acme/main.py", &[("acme.a", 1)], &[])];
        let mut graph = ProjectGraph::new(root.clone());
        let init = with_init.then_some("src/acme/__init__.py");
        for path in PATHS.into_iter().chain(init) {
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

        // Importing `acme.a` also loads the package `__init__.py` itself.
        let init_id = init.and_then(|path| graph.file_id(path));
        let with = |ids: &[FileId]| ids.iter().copied().chain(init_id).collect::<IndexSet<_>>();
        let library_certain = if with_init {
            with(&[main_id])
        } else {
            IndexSet::from([main_id, a_id])
        };
        for (mode, eager, certain) in [
            (
                ProjectMode::App,
                IndexSet::from([main_id]),
                IndexSet::from([main_id]),
            ),
            (
                ProjectMode::Library,
                with(&[main_id, a_id]),
                library_certain,
            ),
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
            assert_eq!(outcome.reachable, with(&[main_id, a_id]));
            assert_eq!(outcome.eager, eager, "{mode:?} {with_init}");
            assert_eq!(outcome.exposed, eager, "{mode:?} {with_init}");
            assert_eq!(outcome.certain, certain, "{mode:?} {with_init}");
        }
    }

    /// #802: a library's public subpackage, or a module its own decorators
    /// register, runs only once a caller imports it, so the files it alone
    /// reaches are exposed but not certain.
    #[test]
    fn certain_set_excludes_files_only_public_modules_reach() {
        let root = test_root();
        let modules = vec![
            parsed("src/acme/main.py", &[("acme.a", 1)], &[]),
            parsed("src/acme/b.py", &[("acme.a", 1)], &[]),
            parsed("src/acme/c.py", &[("acme.b", 1)], &[]),
        ];
        let graph = graph_with_imports(root.clone(), &modules);
        let parse = ParseSummary { modules };
        let module_index = ModuleIndex::build(&graph, &sources(&root));
        let [main_id, a_id, b_id, c_id] = PATHS.map(|path| graph.file_id(path).expect(path));
        let root_at = |path: &str, rule: &str| EntryRoot {
            path: path.to_owned(),
            symbols: BTreeSet::new(),
            context: FileContext::Runtime,
            origins: vec![EntryOrigin::Auto {
                rule: rule.to_owned(),
            }],
        };
        let mut route = PluginContribution::empty(PluginId::Flask);
        route.module_refs.push(ModuleReference {
            module: "acme.c".to_owned(),
            origin: ReferenceOrigin {
                file: "src/acme/c.py".to_owned(),
                line: Some(3),
                label: "flask route decorator".to_owned(),
            },
        });
        let plugins = PluginHints {
            contributions: vec![route],
            ..no_plugins()
        };

        for (mode, certain) in [
            (
                ProjectMode::App,
                IndexSet::from([main_id, a_id, b_id, c_id]),
            ),
            (ProjectMode::Library, IndexSet::from([main_id, a_id])),
        ] {
            let entry = EntryPlan {
                mode,
                roots: vec![
                    root_at("src/acme/main.py", "library package"),
                    root_at("src/acme/b.py", LIBRARY_SUBPACKAGE_RULE),
                ],
                ..entry_plan()
            };
            let outcome =
                run_reachability_bfs(&graph, &entry, &plugins, &parse, &module_index, Vec::new());
            assert_eq!(outcome.exposed.len(), 4, "{mode:?}");
            assert_eq!(outcome.certain, certain, "{mode:?}");
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

    /// The single walk against §6 of the spec read as one BFS per set, the
    /// same reference `Reachability.tla` checks (#860): `eager`, `exposed`
    /// and `certain` are what a plain BFS reaches when it drops the edges
    /// that set may not follow.
    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Files of the model, each the module of the same index but the
        /// last, a test file no module names.
        const FILES: [&str; 6] = [
            "src/acme/__init__.py",
            "src/acme/a.py",
            "src/acme/b.py",
            "src/acme/sub/__init__.py",
            "src/acme/sub/c.py",
            "tests/test_x.py",
        ];
        const MODULES: [&str; 5] = ["acme", "acme.a", "acme.b", "acme.sub", "acme.sub.c"];
        const TEST: usize = 5;
        /// Files under a runtime `__init__.py` of the package.
        const SUBMODULE: [bool; 6] = [false, true, true, true, true, false];

        /// One import site: importing file, module, kind (0 `import`,
        /// 1 `importlib`, 2 `importlib` of the module's name plus a suffix),
        /// lazy (0 no, 1 function-local, 2 `TYPE_CHECKING`), optional (0 no,
        /// 1 `try`, 2 platform guard).
        type Site = (usize, usize, u8, u8, u8);

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum RootKind {
            None,
            Package,
            Public,
        }

        #[derive(Debug, Clone)]
        struct Project {
            sites: Vec<Site>,
            roots: [RootKind; 5],
            test_root: bool,
            library: bool,
            library_member: bool,
            plugin_ref: Option<(usize, usize)>,
            framework: [bool; 6],
        }

        fn project() -> impl Strategy<Value = Project> {
            let root = prop_oneof![
                Just(RootKind::None),
                Just(RootKind::Package),
                Just(RootKind::Public)
            ];
            (
                prop::collection::vec((0..6usize, 0..5usize, 0..3u8, 0..3u8, 0..3u8), 0..12),
                prop::array::uniform5(root),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                prop::option::of((0..5usize, 0..5usize)),
                prop::array::uniform6(any::<bool>()),
            )
                .prop_map(
                    |(sites, roots, test_root, library, library_member, plugin_ref, framework)| {
                        Project {
                            sites,
                            roots,
                            test_root,
                            library,
                            library_member,
                            plugin_ref,
                            framework,
                        }
                    },
                )
        }

        fn targets(module: usize) -> Vec<usize> {
            module_and_parents(MODULES[module])
                .map(|name| MODULES.iter().position(|m| *m == name).expect("module"))
                .collect()
        }

        /// A prefixed site also reaches every module under its package.
        fn site_targets(&(_, module, kind, ..): &Site) -> Vec<usize> {
            let mut found = targets(module);
            if kind == 2 {
                let package = format!("{}.", MODULES[module]);
                found.extend((0..MODULES.len()).filter(|&m| MODULES[m].starts_with(&package)));
            }
            found
        }

        fn parse(project: &Project) -> Vec<ParsedModule> {
            let mut modules: Vec<ParsedModule> =
                FILES.iter().map(|path| parsed(path, &[], &[])).collect();
            for (line, &(from, module, kind, lazy, optional)) in (1u32..).zip(&project.sites) {
                let module = MODULES[module].to_owned();
                let parsed = &mut modules[from];
                if kind != 0 {
                    let dynamic = DynamicImport {
                        module,
                        line,
                        deferred: lazy != 0,
                        optional: optional == 1,
                        platform_guarded: optional == 2,
                        ..DynamicImport::default()
                    };
                    if kind == 1 {
                        parsed.dynamic_imports.push(dynamic);
                    } else {
                        parsed.dynamic_import_prefixes.push(dynamic);
                    }
                } else {
                    let mut import = self::parsed("", &[(module.as_str(), line)], &[])
                        .imports
                        .remove(0);
                    import.deferred = lazy == 1;
                    if lazy == 2 {
                        import.context = ImportContext::Type;
                    }
                    import.optional = optional == 1;
                    import.platform_guarded = optional == 2;
                    parsed.imports.push(import);
                }
            }
            modules
        }

        fn context(index: usize) -> FileContext {
            if index == TEST {
                FileContext::Test
            } else {
                FileContext::Runtime
            }
        }

        fn entry_plan(project: &Project) -> EntryPlan {
            let root_at = |index: usize, public: bool| EntryRoot {
                path: FILES[index].to_owned(),
                symbols: BTreeSet::new(),
                context: context(index),
                origins: if public {
                    vec![EntryOrigin::Auto {
                        rule: LIBRARY_SUBPACKAGE_RULE.to_owned(),
                    }]
                } else {
                    Vec::new()
                },
            };
            let roots = project
                .roots
                .iter()
                .enumerate()
                .filter(|(_, kind)| **kind != RootKind::None)
                .map(|(index, kind)| root_at(index, *kind == RootKind::Public))
                .chain(project.test_root.then(|| root_at(TEST, false)))
                .collect();
            EntryPlan {
                mode: if project.library {
                    ProjectMode::Library
                } else {
                    ProjectMode::App
                },
                library_members: if project.library_member {
                    vec!["src/acme/sub".to_owned()]
                } else {
                    Vec::new()
                },
                distributed_members: Vec::new(),
                roots,
                warnings: Vec::new(),
            }
        }

        fn plugins(project: &Project) -> PluginHints {
            let mut plugins = no_plugins();
            if let Some((origin, module)) = project.plugin_ref {
                let mut contribution = PluginContribution::empty(PluginId::Flask);
                contribution.module_refs.push(ModuleReference {
                    module: MODULES[module].to_owned(),
                    origin: ReferenceOrigin {
                        file: FILES[origin].to_owned(),
                        line: Some(1),
                        label: "flask route decorator".to_owned(),
                    },
                });
                plugins.contributions.push(contribution);
            }
            plugins
        }

        fn run(project: &Project, modules: Vec<ParsedModule>) -> (BfsOutcome, Vec<FileId>) {
            let root = test_root();
            let mut graph = ProjectGraph::new(root.clone());
            for (index, path) in FILES.iter().enumerate() {
                graph
                    .intern_file(FileNode {
                        path: (*path).to_owned(),
                        context: context(index),
                        kind: FileKind::Python,
                    })
                    .expect("file");
            }
            for module in MODULES {
                graph.intern_module(module.to_owned(), ModuleOrigin::FirstParty);
            }
            for parsed in &modules {
                let file_id = graph.file_id(&parsed.path).expect("parsed file");
                add_parsed_imports(&mut graph, file_id, parsed).expect("import edges");
            }
            let ids: Vec<FileId> = FILES
                .iter()
                .map(|path| graph.file_id(path).expect(path))
                .collect();
            let sources = DiscoveredSources {
                files: FILES
                    .iter()
                    .enumerate()
                    .map(|(index, path)| DiscoveredFile {
                        path: (*path).to_owned(),
                        kind: FileKind::Python,
                        context: context(index),
                    })
                    .collect(),
                ..sources(&root)
            };
            let module_index = ModuleIndex::build(&graph, &sources);
            let entry = entry_plan(project);
            let plugins = plugins(project);
            let framework = (0..FILES.len())
                .filter(|index| project.framework[*index])
                .map(|index| {
                    let step = TraceStep::PluginRef {
                        module: "glob".to_owned(),
                        label: "django".to_owned(),
                    };
                    (ids[index], ReachPredecessor { from: None, step })
                })
                .collect();
            let parse = ParseSummary { modules };
            let outcome =
                run_reachability_bfs(&graph, &entry, &plugins, &parse, &module_index, framework);
            (outcome, ids)
        }

        fn is_library(project: &Project, index: usize) -> bool {
            project.library || (project.library_member && FILES[index].starts_with("src/acme/sub/"))
        }

        /// `Follows` in `Reachability.tla`: whether the BFS for the set at
        /// `level` (0 reachable, 1 eager, 2 exposed, 3 certain) follows
        /// `site` into `target`.
        fn follows(project: &Project, level: u8, site: &Site, target: usize) -> bool {
            let &(from, _, _, lazy, optional) = site;
            let test = from == TEST;
            (level < 1 || (lazy == 0 && (!test || is_library(project, target))))
                && (level < 2 || optional == 0)
                && (level < 3 || !test || !SUBMODULE[target])
        }

        fn closure(project: &Project, level: u8, seeds: BTreeSet<usize>) -> BTreeSet<usize> {
            let mut reached = seeds;
            loop {
                let next: BTreeSet<usize> = project
                    .sites
                    .iter()
                    .filter(|site| reached.contains(&site.0))
                    .flat_map(|site| {
                        site_targets(site)
                            .into_iter()
                            .filter(move |target| follows(project, level, site, *target))
                    })
                    .collect();
                if next.is_subset(&reached) {
                    return reached;
                }
                reached.extend(next);
            }
        }

        proptest! {
            #[test]
            fn derived_sets_match_one_bfs_each(project in project()) {
                let (outcome, ids) = run(&project, parse(&project));
                let roots = |wanted: RootKind| {
                    project.roots.iter().enumerate().filter(move |(_, kind)| **kind == wanted)
                        .map(|(index, _)| index)
                };
                // A plugin ref naming the module it sits in, in library mode,
                // is exposed only (#802).
                let (refs, self_registered) = project.plugin_ref.map_or((Vec::new(), false), |(origin, module)| {
                    (targets(module), module == origin && is_library(&project, origin))
                });
                let framework = (0..FILES.len()).filter(|index| project.framework[*index]);
                let seeds: BTreeSet<usize> = roots(RootKind::Package)
                    .chain(roots(RootKind::Public))
                    .chain(project.test_root.then_some(TEST))
                    .chain(refs.iter().copied())
                    .chain(framework.clone())
                    .collect();
                // Roots only `library subpackage` names are exposed only.
                let certain_seeds: BTreeSet<usize> = roots(RootKind::Package)
                    .chain(project.test_root.then_some(TEST))
                    .chain(refs.iter().copied().filter(|_| !self_registered))
                    .chain(framework)
                    .collect();
                let as_ids = |set: BTreeSet<usize>| set.into_iter().map(|index| ids[index].0).collect::<BTreeSet<_>>();
                let sorted = |set: &IndexSet<FileId>| set.iter().map(|id| id.0).collect::<BTreeSet<_>>();
                prop_assert_eq!(sorted(&outcome.reachable), as_ids(closure(&project, 0, seeds.clone())));
                prop_assert_eq!(sorted(&outcome.eager), as_ids(closure(&project, 1, seeds.clone())));
                prop_assert_eq!(sorted(&outcome.exposed), as_ids(closure(&project, 2, seeds)));
                prop_assert_eq!(sorted(&outcome.certain), as_ids(closure(&project, 3, certain_seeds)));
                for file_id in &outcome.reachable {
                    prop_assert!(outcome.predecessors.contains_key(file_id));
                }
            }
        }
    }
}
