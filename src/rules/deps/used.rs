//! Build the set of used distributions from imports, plugins, and binaries.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use indexmap::IndexSet;

use super::companions::used_modules;
use crate::graph::{FileId, ModuleOrigin, ProjectGraph};
use crate::manifest::{LoadedManifest, LockfileGraph, normalize_distribution_name};
use crate::plugins::PluginHints;
use crate::reachability::ReachabilityReport;
use crate::resolver::{ResolutionIndex, import_root};
use crate::rules::types::WorkspaceDependencyBoundary;
use crate::rules::{DependencyRuleContext, RuleContext};
use crate::sources::{DiscoveredFile, LocalTrees, importable_modules};

/// Index of declared dependencies keyed by normalized distribution name.
pub(super) type DeclaredIndex<'a> = BTreeMap<String, Vec<&'a crate::manifest::DeclaredDependency>>;

/// Build declared dependency index from manifest.
pub(super) fn build_declared_index(manifest: &LoadedManifest) -> DeclaredIndex<'_> {
    let mut index: DeclaredIndex<'_> = BTreeMap::new();
    for dep in &manifest.dependencies {
        index.entry(dep.name.clone()).or_default().push(dep);
    }
    index
}

/// [`build_declared_index`] plus the distributions `lockfile` says a
/// declared `pkg[extra]` adds, each indexed under the declaration that
/// requested it: the user asked for them explicitly, so importing one is not
/// CHK003 (#516).
pub(super) fn build_import_declared_index<'a>(
    manifest: &'a LoadedManifest,
    lockfile: &LockfileGraph,
) -> DeclaredIndex<'a> {
    let mut index = build_declared_index(manifest);
    for dep in &manifest.dependencies {
        let Some(extras) = lockfile.extras.get(&dep.name) else {
            continue;
        };
        for extra in &dep.extras {
            for name in extras
                .get(&normalize_distribution_name(extra))
                .into_iter()
                .flatten()
            {
                index.entry(name.clone()).or_default().push(dep);
            }
        }
    }
    index
}

/// Collect root-relative paths of reachable Python files.
pub(super) fn reachable_paths<'g>(
    graph: &'g ProjectGraph,
    reachability: &ReachabilityReport,
) -> HashSet<&'g str> {
    reachability
        .reachable
        .iter()
        .filter_map(|file_id| graph.file(*file_id).map(|node| node.path.as_str()))
        .collect()
}

/// Paths of `files`, such as [`ReachabilityReport::eager`].
pub(super) fn file_paths<'g>(
    graph: &'g ProjectGraph,
    files: &IndexSet<FileId>,
) -> HashSet<&'g str> {
    files
        .iter()
        .filter_map(|file_id| graph.file(*file_id).map(|node| node.path.as_str()))
        .collect()
}

/// Reachable files plus library orphans an outside caller may import: the
/// files whose imports keep a declared dependency from reading as unused.
///
/// A library's public modules are its entry points, so a library with no
/// script, test, or config root would otherwise report every runtime
/// dependency unused (#501). Those orphans are the ones still capped at
/// `Maybe` after the wheel public surface re-scored the rest. Only CHK002
/// uses this set: an orphan's import is no proof the module ships, so it
/// must not raise CHK003-CHK005.
pub(super) fn usage_paths<'a>(
    reachable: &HashSet<&'a str>,
    reachability: &'a ReachabilityReport,
) -> HashSet<&'a str> {
    let public_orphans = reachability
        .unreachable
        .iter()
        .filter(|file| file.callers_may_import())
        .map(|file| file.path.as_str());
    reachable.iter().copied().chain(public_orphans).collect()
}

/// Whether the project has lockfile data for transitive checks.
#[must_use]
pub(super) fn has_lockfile(manifest: &LoadedManifest, resolution: &ResolutionIndex) -> bool {
    manifest.sources.lockfile.is_some() || !resolution.transitive.edges.is_empty()
}

/// Distributions used by reachable imports, plugin refs, and binaries.
pub(super) fn collect_used_distributions(
    context: &RuleContext<'_>,
    plugins: &PluginHints,
    reachable: &HashSet<&str>,
) -> IndexSet<String> {
    let RuleContext {
        resolution, graph, ..
    } = *context;
    let binary_resolutions = &resolution.binary_resolutions;
    let mut used = IndexSet::new();

    for import in &resolution.imports {
        if import.origin != ModuleOrigin::ThirdParty {
            continue;
        }
        let Some(distribution) = import.distribution.as_ref() else {
            continue;
        };
        if !counts_as_use(import.file.as_str(), reachable, graph) {
            continue;
        }
        used.insert(distribution.clone());
    }

    for usage in plugins.all_binary_usages() {
        if let Some(distribution) = binary_resolutions.get(&usage.binary) {
            used.insert(distribution.clone());
        }
    }

    used
}

/// Modules the reachable imports and plugin refs use, whose runtime peers
/// they load (`DATABASES` naming Django's Postgres backend).
pub(super) fn used_third_party_modules(
    context: &RuleContext<'_>,
    reachable: &HashSet<&str>,
) -> BTreeSet<String> {
    let counts = |file: &str| counts_as_use(file, reachable, context.graph);
    used_modules(
        context
            .resolution
            .imports
            .iter()
            .map(AsRef::as_ref)
            .filter(|import| counts(&import.file)),
        context
            .parse
            .modules
            .iter()
            .filter(|module| counts(&module.path)),
    )
}

/// Whether an import in `file` is a use: the file is reachable, or it is a
/// config file outside the graph whose plugin refs (pytest `-p`, mypy
/// plugins) name modules no BFS reaches.
fn counts_as_use(file: &str, reachable: &HashSet<&str>, graph: &ProjectGraph) -> bool {
    reachable.contains(file) || graph.file_id(file).is_none()
}

/// `workspace = true` dependencies, and `path` sources that hold the project's
/// own package (`streamlit = { path = "lib" }`), resolve as first-party
/// imports, so they carry no distribution; match them back by normalized import name, or by
/// the member tree that holds the imported module (`airflow` from
/// `airflow-core/src/airflow` uses `apache-airflow-core`). The manifest's own
/// entry point targets count as such imports too, and so do the imports of a
/// used member's own files: `airflow-core` importing `airflow.sdk` uses
/// `apache-airflow-task-sdk`, since the root ships both trees. Those imports
/// may resolve to a distribution (`airflow` maps to `apache-airflow`), so only
/// the member tree decides.
pub(super) fn mark_workspace_source_distributions(
    manifest: &LoadedManifest,
    dependency: &DependencyRuleContext<'_>,
    reachable: &HashSet<&str>,
    workspace_boundaries: &[WorkspaceDependencyBoundary<'_>],
    used: &mut IndexSet<String>,
) {
    let DependencyRuleContext {
        rules: context,
        local,
        ..
    } = *dependency;
    let first_party: Vec<&str> = context
        .resolution
        .imports
        .iter()
        .filter(|import| {
            import.origin == ModuleOrigin::FirstParty
                && counts_as_use(import.file.as_str(), reachable, context.graph)
        })
        .map(|import| import.full_module.as_str())
        .collect();
    let entry_modules: Vec<&str> = manifest
        .entry_points
        .iter()
        .filter_map(|entry| entry.target.split(':').next())
        .map(str::trim)
        .collect();
    for module in first_party.iter().chain(&entry_modules) {
        let name = normalize_distribution_name(import_root(module));
        if local.is_local_source(&name) {
            used.insert(name);
        }
    }

    let paths = sorted_paths(&context.sources.files);
    let mut members = Vec::new();
    for boundary in workspace_boundaries {
        let Some(name) = local_source_member(manifest, local, boundary.manifest) else {
            continue;
        };
        let Some(member_path) = member_path(manifest, boundary.manifest) else {
            continue;
        };
        // A root that inventories only its own package (`src/**`) misses the
        // member's files, so the member's inventory supplies its tree (#559).
        let mut provided = member_modules(&member_path, under(&paths, &member_path));
        provided.extend(member_modules(
            "",
            boundary.files.iter().map(|file| file.path.as_str()),
        ));
        members.push((name, member_path, boundary.imports, provided));
    }
    if members.is_empty() {
        return;
    }

    let providers = Providers::new(members.iter().map(|(_, _, _, provided)| provided));
    let member_imports = MemberImports::new(context.resolution);

    let mut provided = vec![false; members.len()];
    let mut done = vec![false; members.len()];
    let mut modules: HashSet<&str> = HashSet::new();
    let mut found: Vec<&str> = first_party.iter().chain(&entry_modules).copied().collect();
    // Each round marks, in declaration order, the members the modules found
    // by the end of the previous round use; their trees' imports feed the
    // next round.
    loop {
        for module in std::mem::take(&mut found) {
            if !modules.insert(module) {
                continue;
            }
            for index in providers.of(module) {
                provided[index] = true;
            }
        }
        let now_used: Vec<usize> = (0..members.len())
            .filter(|&index| !done[index] && (provided[index] || used.contains(&members[index].0)))
            .collect();
        if now_used.is_empty() {
            break;
        }
        for index in now_used {
            done[index] = true;
            let (name, member_path, imports, _) = &members[index];
            if !member_path.is_empty() {
                found.extend(member_imports.under(member_path));
                found.extend(imports.iter().map(String::as_str));
            }
            used.insert(name.clone());
        }
    }
}

/// Inventory paths in byte order, so the files under a directory form one range.
pub(super) fn sorted_paths(files: &[DiscoveredFile]) -> Vec<&str> {
    let mut paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    paths.sort_unstable();
    paths
}

/// The `paths` under `member_path`; all of them for the root itself.
fn under<'a>(paths: &[&'a str], member_path: &str) -> impl Iterator<Item = &'a str> {
    if member_path.is_empty() {
        return paths.iter().copied();
    }
    within(paths, |path| path, member_path).iter().copied()
}

/// The run of `items`, sorted by `path`, whose path is under the non-empty
/// `member_path`.
pub(super) fn within<'s, T>(
    items: &'s [T],
    path: impl Fn(&T) -> &str,
    member_path: &str,
) -> &'s [T] {
    let prefix = format!("{member_path}/");
    let start = items.partition_point(|item| path(item) < prefix.as_str());
    let len = items[start..]
        .iter()
        .take_while(|item| path(item).starts_with(&prefix))
        .count();
    &items[start..start + len]
}

/// Non-stdlib imports as `(file, full module)`, sorted by file and otherwise
/// in resolution order.
struct MemberImports<'a>(Vec<(&'a str, &'a str)>);

impl<'a> MemberImports<'a> {
    fn new(resolution: &'a ResolutionIndex) -> Self {
        let mut imports: Vec<(&str, &str)> = resolution
            .imports
            .iter()
            .filter(|import| import.origin != ModuleOrigin::Stdlib)
            .map(|import| (import.file.as_str(), import.full_module.as_str()))
            .collect();
        imports.sort_by_key(|(file, _)| *file);
        Self(imports)
    }

    /// Modules imported by files under a non-root member tree.
    fn under(&self, member_path: &str) -> impl Iterator<Item = &'a str> + use<'a, '_> {
        within(&self.0, |(file, _)| file, member_path)
            .iter()
            .map(|(_, module)| *module)
    }
}

/// Normalized name of `member` when `root` declares it and takes it from a
/// local `workspace` / `path` source.
pub(crate) fn local_source_member(
    root: &LoadedManifest,
    local: &LocalTrees,
    member: &LoadedManifest,
) -> Option<String> {
    let name = normalize_distribution_name(member.metadata.name.as_deref()?);
    let declared = root
        .dependencies
        .iter()
        .any(|dep| normalize_distribution_name(&dep.name) == name);
    (declared && local.is_local_source(&name)).then_some(name)
}

/// Root-relative `/` path of a workspace member; empty for the root itself.
fn member_path(root: &LoadedManifest, member: &LoadedManifest) -> Option<String> {
    let relative = member.root.path.strip_prefix(&root.root.path).ok()?;
    let parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// The members that provide each module, from their [`member_modules`].
struct Providers<'a> {
    by_module: HashMap<&'a str, Vec<usize>>,
    /// Keyed by a module a member file holds itself.
    by_parent: HashMap<&'a str, Vec<usize>>,
}

impl<'a> Providers<'a> {
    fn new(members: impl IntoIterator<Item = &'a HashMap<String, bool>>) -> Self {
        let mut by_module: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut by_parent: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, provided) in members.into_iter().enumerate() {
            for (module, holds_file) in provided {
                by_module.entry(module).or_default().push(index);
                if *holds_file {
                    by_parent.entry(module).or_default().push(index);
                }
            }
        }
        Self {
            by_module,
            by_parent,
        }
    }

    /// Members whose modules include `module`, or that hold the module
    /// `module` names something in (`from acme_util import y` is
    /// `acme_util.y`). A namespace package such as `airflow` in a provider
    /// does not count as a parent, since other members provide it as well.
    fn of(&self, module: &str) -> impl Iterator<Item = usize> + use<'_> {
        let parent = module.rsplit_once('.').map(|(parent, _)| parent);
        self.by_module
            .get(module)
            .into_iter()
            .chain(parent.and_then(|parent| self.by_parent.get(parent)))
            .flatten()
            .copied()
    }
}

/// Dotted module names the files under `member_path` provide, by
/// [`importable_modules`] from the member root or from any `src` directory
/// below it (`dev/breeze/src/airflow_breeze` gives `airflow_breeze`). Wider
/// than the resolver's roots: a missed module reports a used member unused.
fn member_modules<'a>(
    member_path: &str,
    paths: impl IntoIterator<Item = &'a str>,
) -> HashMap<String, bool> {
    let paths: Vec<&str> = paths
        .into_iter()
        .filter(|path| {
            member_path.is_empty()
                || path
                    .strip_prefix(member_path)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .collect();
    let mut roots = HashSet::from([member_path]);
    for path in &paths {
        roots.extend(
            path[member_path.len()..]
                .match_indices('/')
                .map(|(slash, _)| &path[..member_path.len() + slash])
                .filter(|dir| *dir == "src" || dir.ends_with("/src")),
        );
    }
    importable_modules(paths, &roots)
}

/// Treat `pytest11` plugins as used whenever pytest is, since pytest loads
/// them without any import or config reference: the ones installed in the
/// venv, and the declared `pytest-*` names the plugin naming convention gives
/// them, which is all there is without a venv (#809).
pub(super) fn mark_pytest_plugin_distributions(
    resolution: &ResolutionIndex,
    declared: &DeclaredIndex<'_>,
    used: &mut IndexSet<String>,
) {
    if !used.contains("pytest") {
        return;
    }
    let named = declared.keys().filter(|name| name.starts_with("pytest-"));
    used.extend(
        resolution
            .pytest_plugin_distributions
            .iter()
            .chain(named)
            .cloned(),
    );
}

/// Treat a project's own distribution as used when declared (self-referential extras).
pub(super) fn mark_self_referential_distribution(
    manifest: &LoadedManifest,
    declared: &DeclaredIndex<'_>,
    used: &mut IndexSet<String>,
) {
    let Some(project_name) = manifest.metadata.name.as_ref() else {
        return;
    };
    let normalized = crate::manifest::normalize_distribution_name(project_name);
    if declared.contains_key(&normalized) {
        used.insert(normalized);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::{Confidence, ProjectMode};
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::{FileNode, ProjectGraph};
    use crate::manifest::{LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata};
    use crate::parser::ImportContext;
    use crate::reachability::ReachabilityReport;
    use crate::resolver::{ResolveConfidence, ResolvedImport};

    fn mark(
        manifest: &LoadedManifest,
        rules: &RuleContext<'_>,
        reachable: &HashSet<&str>,
        boundaries: &[WorkspaceDependencyBoundary<'_>],
    ) -> IndexSet<String> {
        let config = crate::config::default_config();
        let mut used = IndexSet::new();
        mark_workspace_source_distributions(
            manifest,
            &DependencyRuleContext {
                rules,
                config: &config,
                strict: false,
                build_scripts: &BTreeMap::new(),
                local: &LocalTrees::new(manifest, &[], &config),
            },
            reachable,
            boundaries,
            &mut used,
        );
        used
    }

    #[test]
    fn detects_lockfile_from_manifest_sources() {
        let manifest = LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings::default(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources {
                lockfile: Some(crate::manifest::LockfileSource {
                    kind: crate::manifest::LockfileKind::Uv,
                    path: "uv.lock".to_owned(),
                }),
                ..ManifestSources::default()
            },
            warnings: Vec::new(),
        };
        let resolution = ResolutionIndex::default();
        assert!(has_lockfile(&manifest, &resolution));
    }

    #[test]
    fn collects_third_party_from_reachable_import() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        });
        let file_id = graph
            .intern_file(FileNode {
                path: "src/app.py".to_owned(),
                context: crate::sources::FileContext::Runtime,
                kind: crate::sources::FileKind::Python,
            })
            .expect("file id");
        let reachable = {
            let mut report = ReachabilityReport::default();
            report.reachable.insert(file_id);
            report
        };
        let resolution = ResolutionIndex {
            imports: vec![Arc::new(ResolvedImport {
                import_root: "yaml".to_owned(),
                full_module: "yaml".to_owned(),
                file: "src/app.py".to_owned(),
                workspace_member: None,
                line: Some(1),
                reference: None,
                context: ImportContext::Runtime,
                optional: false,
                platform_guarded: false,
                never_runs: false,
                origin: ModuleOrigin::ThirdParty,
                distribution: Some("pyyaml".to_owned()),
                confidence: ResolveConfidence::Certain,
            })],
            warnings: Vec::new(),
            transitive: LockfileGraph::default(),
            binary_resolutions: BTreeMap::new(),
            pytest_plugin_distributions: std::collections::BTreeSet::new(),
        };
        let sources = crate::sources::DiscoveredSources {
            root: graph.root.clone(),
            layout: crate::sources::LayoutInfo::new("src", Vec::new()),
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let used = collect_used_distributions(
            &RuleContext {
                resolution: &resolution,
                reachability: &reachable,
                graph: &graph,
                sources: &sources,
                parse: &crate::parser::ParseSummary::default(),
                surface: None,
            },
            &PluginHints {
                contributions: Vec::new(),
                config_binary_usages: Vec::new(),
                config_used_distributions: Vec::new(),
                config_declared_distributions: Vec::new(),
                config_provided_binaries: Vec::new(),
                config_module_refs: Vec::new(),
                config_used_symbols: Vec::new(),
                config_script_entries: Vec::new(),
                config_command_targets: Vec::new(),
                warnings: Vec::new(),
            },
            &reachable_paths(&graph, &reachable),
        );
        assert!(used.contains("pyyaml"));
    }

    #[test]
    fn marks_self_referential_distribution_as_used() {
        let dep = crate::manifest::DeclaredDependency {
            name: "self-extra".to_owned(),
            extras: vec!["benchmark".to_owned()],
            marker: None,
            specifier: None,
            context: crate::manifest::DependencyContext::Runtime,
            origin: crate::manifest::DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(4),
                label: "project.dependencies[0]".to_owned(),
            },
            opaque: false,
            included_via: Vec::new(),
            unportable_key: None,
        };
        let manifest = LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            metadata: ProjectMetadata {
                name: Some("self-extra".to_owned()),
                ..ProjectMetadata::default()
            },
            dependencies: vec![dep],
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings::default(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        };
        let declared = build_declared_index(&manifest);
        let mut used = IndexSet::new();
        mark_self_referential_distribution(&manifest, &declared, &mut used);
        assert!(used.contains("self-extra"));
    }

    #[test]
    fn member_modules_start_at_member_root_or_src() {
        let files = [
            python_file("airflow-core/src/airflow/__init__.py"),
            python_file("airflow-core/src/airflow/models/dag.py"),
            python_file("airflow-core/tests/utils.py"),
            python_file("task-sdk/src/airflow/sdk/__init__.py"),
        ];
        let modules = member_modules("airflow-core", paths(&files));
        assert_eq!(modules.get("airflow"), Some(&true));
        assert_eq!(modules.get("airflow.models"), Some(&false));
        assert_eq!(modules.get("airflow.models.dag"), Some(&true));
        assert!(modules.contains_key("tests.utils"));
        assert!(!modules.contains_key("utils"));
        assert!(!modules.contains_key("models"));
        assert!(!modules.contains_key("airflow.sdk"));
        assert!(!modules.contains_key(""));
        let providers = Providers::new([&modules]);
        let provided = |module: &str| providers.of(module).next().is_some();
        assert!(provided("airflow.models.dag.DAG"));
        assert!(provided("airflow.DAG"));
        assert!(!provided("airflow.models.DAG"));
    }

    #[test]
    fn member_modules_start_after_a_nested_src() {
        // airflow's `dev` member keeps breeze under `dev/breeze/src`.
        let files = [
            python_file("dev/breeze/src/airflow_breeze/__init__.py"),
            python_file("dev/breeze/src/airflow_breeze/cli.py"),
            python_file("tools/src/other/__init__.py"),
        ];
        let modules = member_modules("dev", paths(&files));
        assert_eq!(modules.get("airflow_breeze"), Some(&true));
        assert_eq!(modules.get("airflow_breeze.cli"), Some(&true));
        assert!(modules.contains_key("breeze.src.airflow_breeze"));
        assert!(!modules.contains_key("other"));
        let root = member_modules("", paths(&files));
        assert_eq!(root.get("airflow_breeze.cli"), Some(&true));
        assert_eq!(root.get("other"), Some(&true));
        // A `src` above the member is not one of its roots.
        let nested = [python_file("src/billing/billing/core.py")];
        let modules = member_modules("src/billing", paths(&nested));
        assert_eq!(modules.get("billing.core"), Some(&true));
        assert!(!modules.contains_key("billing.billing"));
    }

    fn paths(files: &[DiscoveredFile]) -> impl Iterator<Item = &str> {
        files.iter().map(|file| file.path.as_str())
    }

    fn python_file(path: &str) -> DiscoveredFile {
        DiscoveredFile {
            path: path.to_owned(),
            kind: crate::sources::FileKind::Python,
            context: crate::sources::FileContext::Runtime,
        }
    }

    /// A graph holding every importing file, so none reads as a config file.
    fn graph_of(manifest: &LoadedManifest, resolution: &ResolutionIndex) -> ProjectGraph {
        let mut graph = ProjectGraph::new(manifest.root.clone());
        for import in &resolution.imports {
            if graph.file_id(&import.file).is_some() {
                continue;
            }
            graph
                .intern_file(FileNode {
                    path: import.file.clone(),
                    context: crate::sources::FileContext::Runtime,
                    kind: crate::sources::FileKind::Python,
                })
                .expect("file id");
        }
        graph
    }

    fn resolved(file: &str, module: &str, origin: ModuleOrigin) -> ResolvedImport {
        ResolvedImport {
            import_root: import_root(module).to_owned(),
            full_module: module.to_owned(),
            file: file.to_owned(),
            workspace_member: None,
            line: Some(1),
            reference: None,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            never_runs: false,
            origin,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        }
    }

    #[test]
    fn member_imports_stay_in_member_tree() {
        let resolution = ResolutionIndex {
            imports: vec![
                Arc::new(resolved(
                    "airflow-core/src/airflow/models/dag.py",
                    "airflow.sdk",
                    ModuleOrigin::FirstParty,
                )),
                Arc::new(resolved(
                    "airflow-core/src/airflow/models/dag.py",
                    "airflow.sdk.definitions.dag",
                    ModuleOrigin::ThirdParty,
                )),
                Arc::new(resolved(
                    "airflow-core/src/airflow/models/dag.py",
                    "os",
                    ModuleOrigin::Stdlib,
                )),
                Arc::new(resolved(
                    "airflow-core-extra/src/extra.py",
                    "extra.api",
                    ModuleOrigin::FirstParty,
                )),
            ],
            warnings: Vec::new(),
            transitive: LockfileGraph::default(),
            binary_resolutions: BTreeMap::new(),
            pytest_plugin_distributions: std::collections::BTreeSet::new(),
        };
        let modules: Vec<&str> = MemberImports::new(&resolution)
            .under("airflow-core")
            .collect();
        assert_eq!(modules, ["airflow.sdk", "airflow.sdk.definitions.dag"]);
    }

    #[test]
    fn within_keeps_every_path_that_starts_with_the_member_directory() {
        let paths = ["pkg", "pkg-extra/a.py", "pkg/", "pkg/a.py", "pkgx/b.py"];
        assert_eq!(within(&paths, |path| path, "pkg"), ["pkg/", "pkg/a.py"]);
    }

    fn manifest_at(
        path: std::path::PathBuf,
        name: &str,
        dependencies: &[&str],
        workspace_sources: &[&str],
    ) -> LoadedManifest {
        let origin = crate::manifest::DependencyOrigin {
            file: "pyproject.toml".to_owned(),
            line: Some(1),
            label: "project.dependencies[0]".to_owned(),
        };
        LoadedManifest {
            root: ProjectRoot {
                path,
                marker: RootMarker::PyProjectToml,
            },
            metadata: ProjectMetadata {
                name: Some(name.to_owned()),
                ..ProjectMetadata::default()
            },
            dependencies: dependencies
                .iter()
                .map(|dep| crate::manifest::DeclaredDependency {
                    name: (*dep).to_owned(),
                    extras: Vec::new(),
                    marker: None,
                    specifier: None,
                    context: crate::manifest::DependencyContext::Runtime,
                    origin: origin.clone(),
                    opaque: false,
                    included_via: Vec::new(),
                    unportable_key: None,
                })
                .collect(),
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings {
                build_constraints: std::collections::BTreeSet::new(),
                sources: workspace_sources
                    .iter()
                    .map(|source| crate::manifest::UvSource {
                        name: (*source).to_owned(),
                        kind: crate::manifest::UvSourceKind::Workspace,
                    })
                    .collect(),
            },
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn no_lockfile_and_no_transitive_edges_disables_lock_checks() {
        let manifest = manifest_at(std::env::temp_dir(), "app", &[], &[]);
        assert!(!has_lockfile(&manifest, &ResolutionIndex::default()));
        let resolution = ResolutionIndex {
            transitive: LockfileGraph {
                edges: BTreeMap::from([("requests".to_owned(), vec!["urllib3".to_owned()])]),
                ..LockfileGraph::default()
            },
            ..ResolutionIndex::default()
        };
        assert!(has_lockfile(&manifest, &resolution));
    }

    /// `core` is used through its module tree, `sdk` only through an import
    /// inside `core`, and `util` only through an import in an `sdk` file the
    /// root does not inventory (#712). `extra` is imported only by an
    /// unreachable file, so `lone`, which only `extra` imports, stays unused.
    /// `stray` is not declared, and `plain` has no workspace source.
    #[test]
    fn workspace_sources_are_used_through_member_trees() {
        let root = std::env::temp_dir().join("ws");
        let manifest = manifest_at(
            root.clone(),
            "root",
            &["core", "sdk", "extra", "plain", "util", "lone"],
            &["core", "sdk", "extra", "stray", "util", "lone"],
        );
        let members = ["core", "sdk", "extra", "stray", "plain", "util", "lone"]
            .map(|name| (name, manifest_at(root.join(name), name, &[], &[])));
        let imports = |name: &str| match name {
            "sdk" => vec!["utilpkg.helpers".to_owned()],
            "extra" => vec!["lonepkg".to_owned()],
            _ => Vec::new(),
        };
        let member_imports = members.each_ref().map(|(name, _)| imports(name));
        let boundaries: Vec<WorkspaceDependencyBoundary<'_>> = members
            .iter()
            .zip(&member_imports)
            .map(|((name, member), imports)| WorkspaceDependencyBoundary {
                member_id: name,
                manifest: member,
                files: &[],
                imports,
            })
            .collect();
        let import = |file: &str, module: &str| resolved(file, module, ModuleOrigin::FirstParty);
        let resolution = ResolutionIndex {
            imports: vec![
                Arc::new(import("src/app.py", "corepkg.models")),
                Arc::new(import("src/app.py", "straypkg")),
                Arc::new(import("src/app.py", "plainpkg")),
                Arc::new(import("scripts/old.py", "extrapkg")),
                Arc::new(import("core/src/corepkg/models.py", "sdkpkg")),
            ],
            ..ResolutionIndex::default()
        };
        let sources = crate::sources::DiscoveredSources {
            root: manifest.root.clone(),
            layout: crate::sources::LayoutInfo::new("src", Vec::new()),
            effective_globs: Vec::new(),
            files: members
                .iter()
                .map(|(name, _)| python_file(&format!("{name}/src/{name}pkg/__init__.py")))
                .chain([python_file("core/src/corepkg/models.py")])
                .collect(),
            warnings: Vec::new(),
        };
        let graph = graph_of(&manifest, &resolution);
        let used = mark(
            &manifest,
            &RuleContext {
                resolution: &resolution,
                reachability: &ReachabilityReport::default(),
                graph: &graph,
                sources: &sources,
                parse: &crate::parser::ParseSummary::default(),
                surface: None,
            },
            &HashSet::from(["src/app.py"]),
            &boundaries,
        );
        let mut used: Vec<String> = used.into_iter().collect();
        used.sort();
        assert_eq!(used, ["core", "sdk", "util"]);
    }

    /// Members are marked in rounds: each round adds, in declaration order,
    /// the members the modules found so far use. `omega` shares `alphapkg`
    /// only as a namespace parent, so `alphapkg.thing` does not reach it.
    #[test]
    fn workspace_sources_are_marked_in_discovery_rounds() {
        let names = ["omega", "delta", "gamma", "beta", "alpha"];
        let root = std::env::temp_dir().join("rounds");
        let manifest = manifest_at(root.clone(), "root", &names, &names);
        let members = names.map(|name| (name, manifest_at(root.join(name), name, &[], &[])));
        let boundaries: Vec<WorkspaceDependencyBoundary<'_>> = members
            .iter()
            .map(|(name, member)| WorkspaceDependencyBoundary {
                member_id: name,
                manifest: member,
                files: &[],
                imports: &[],
            })
            .collect();
        let import = |file: &str, module: &str| resolved(file, module, ModuleOrigin::FirstParty);
        let resolution = ResolutionIndex {
            imports: vec![
                Arc::new(import("src/app.py", "alphapkg.thing")),
                Arc::new(import("alpha/src/alphapkg/__init__.py", "betapkg")),
                Arc::new(import("alpha/src/alphapkg/__init__.py", "gammapkg")),
                Arc::new(import("beta/src/betapkg/__init__.py", "deltapkg")),
            ],
            ..ResolutionIndex::default()
        };
        let sources = crate::sources::DiscoveredSources {
            root: manifest.root.clone(),
            layout: crate::sources::LayoutInfo::new("src", Vec::new()),
            effective_globs: Vec::new(),
            files: names
                .iter()
                .filter(|name| **name != "omega")
                .map(|name| python_file(&format!("{name}/src/{name}pkg/__init__.py")))
                .chain([python_file("omega/src/alphapkg/sub/mod.py")])
                .collect(),
            warnings: Vec::new(),
        };
        let graph = graph_of(&manifest, &resolution);
        let used = mark(
            &manifest,
            &RuleContext {
                resolution: &resolution,
                reachability: &ReachabilityReport::default(),
                graph: &graph,
                sources: &sources,
                parse: &crate::parser::ParseSummary::default(),
                surface: None,
            },
            &HashSet::from(["src/app.py"]),
            &boundaries,
        );
        assert_eq!(
            used.into_iter().collect::<Vec<_>>(),
            ["alpha", "gamma", "beta", "delta"]
        );
    }

    /// `streamlit = { path = "lib" }` holds the project's own package, so a
    /// first-party `streamlit` import uses that dependency.
    #[test]
    fn path_source_is_used_through_first_party_imports() {
        let mut manifest = manifest_at(
            std::env::temp_dir().join("dev"),
            "streamlit-dev",
            &["streamlit"],
            &[],
        );
        manifest.uv.sources.push(crate::manifest::UvSource {
            name: "streamlit".to_owned(),
            kind: crate::manifest::UvSourceKind::Path("lib".to_owned()),
        });
        let resolution = ResolutionIndex {
            imports: vec![Arc::new(ResolvedImport {
                import_root: "streamlit".to_owned(),
                full_module: "streamlit.web".to_owned(),
                file: "e2e/app.py".to_owned(),
                workspace_member: None,
                line: Some(1),
                reference: None,
                context: ImportContext::Runtime,
                optional: false,
                platform_guarded: false,
                never_runs: false,
                origin: ModuleOrigin::FirstParty,
                distribution: None,
                confidence: ResolveConfidence::Certain,
            })],
            ..ResolutionIndex::default()
        };
        let sources = crate::sources::DiscoveredSources {
            root: manifest.root.clone(),
            layout: crate::sources::LayoutInfo::new("lib", vec!["streamlit".to_owned()]),
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let graph = graph_of(&manifest, &resolution);
        let used = mark(
            &manifest,
            &RuleContext {
                resolution: &resolution,
                reachability: &ReachabilityReport::default(),
                graph: &graph,
                sources: &sources,
                parse: &crate::parser::ParseSummary::default(),
                surface: None,
            },
            &HashSet::from(["e2e/app.py"]),
            &[],
        );
        assert!(used.contains("streamlit"));
    }

    /// Issue #509: a `project.scripts` target inside the path source uses it
    /// with no import of it; an unreached path source stays unused.
    #[test]
    fn path_source_is_used_through_entry_points() {
        let mut manifest = manifest_at(
            std::env::temp_dir().join("dev"),
            "acme-dev",
            &["acme", "other"],
            &[],
        );
        for name in ["acme", "other"] {
            manifest.uv.sources.push(crate::manifest::UvSource {
                name: name.to_owned(),
                kind: crate::manifest::UvSourceKind::Path(format!("{name}-lib")),
            });
        }
        manifest.entry_points.push(crate::manifest::EntryPointDecl {
            name: "acme".to_owned(),
            target: "acme.cli:main".to_owned(),
            group: "console".to_owned(),
            origin: crate::manifest::DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: None,
                label: "project.scripts.acme".to_owned(),
            },
        });
        let sources = crate::sources::DiscoveredSources {
            root: manifest.root.clone(),
            layout: crate::sources::LayoutInfo::new("", Vec::new()),
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let graph = ProjectGraph::new(manifest.root.clone());
        let used = mark(
            &manifest,
            &RuleContext {
                resolution: &ResolutionIndex::default(),
                reachability: &ReachabilityReport::default(),
                graph: &graph,
                sources: &sources,
                parse: &crate::parser::ParseSummary::default(),
                surface: None,
            },
            &HashSet::new(),
            &[],
        );
        assert_eq!(used.into_iter().collect::<Vec<_>>(), ["acme"]);
    }

    #[test]
    fn usage_paths_adds_only_library_orphans_capped_at_maybe() {
        let orphan = |path: &str, mode, max_confidence| crate::reachability::UnreachableFile {
            file: crate::graph::FileId(0),
            path: path.to_owned(),
            max_confidence,
            mode,
            confidence_reason: None,
        };
        let mut report = ReachabilityReport::default();
        report.unreachable = vec![
            orphan("public.py", ProjectMode::Library, Confidence::Maybe),
            orphan("outside_wheel.py", ProjectMode::App, Confidence::Certain),
            orphan("app_orphan.py", ProjectMode::App, Confidence::Maybe),
        ];
        let reachable = HashSet::from(["main.py"]);

        let usage = usage_paths(&reachable, &report);

        assert_eq!(usage, HashSet::from(["main.py", "public.py"]));
    }
}
