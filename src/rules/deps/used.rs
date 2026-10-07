//! Build the set of used distributions from imports, plugins, and binaries.

use std::collections::{BTreeMap, HashSet};

use indexmap::IndexSet;

use crate::config::{Confidence, ProjectMode};
use crate::graph::{ModuleOrigin, ProjectGraph};
use crate::manifest::{LoadedManifest, LockfileGraph, normalize_distribution_name};
use crate::plugins::PluginHints;
use crate::reachability::ReachabilityReport;
use crate::resolver::{ResolutionIndex, import_root};
use crate::rules::RuleContext;
use crate::rules::types::WorkspaceDependencyBoundary;
use crate::sources::DiscoveredFile;

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

/// Paths of [`ReachabilityReport::eager`].
pub(super) fn eager_paths<'g>(
    graph: &'g ProjectGraph,
    reachability: &ReachabilityReport,
) -> HashSet<&'g str> {
    reachability
        .eager
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
        .filter(|file| {
            file.mode == ProjectMode::Library && file.max_confidence == Confidence::Maybe
        })
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
        // Plugin refs read from config files (pytest `-p`, mypy plugins) name a
        // file outside the graph; those count even though no BFS reaches them.
        if !reachable.contains(import.file.as_str()) && graph.file_id(&import.file).is_some() {
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
    context: &RuleContext<'_>,
    reachable: &HashSet<&str>,
    workspace_boundaries: &[WorkspaceDependencyBoundary<'_>],
    used: &mut IndexSet<String>,
) {
    let first_party: Vec<&str> = context
        .resolution
        .imports
        .iter()
        .filter(|import| {
            import.origin == ModuleOrigin::FirstParty && reachable.contains(import.file.as_str())
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
        if manifest.uv.is_local_source(&name) {
            used.insert(name);
        }
    }

    let declared: HashSet<String> = manifest
        .dependencies
        .iter()
        .map(|dep| normalize_distribution_name(&dep.name))
        .collect();
    let mut modules: HashSet<&str> = first_party.iter().chain(&entry_modules).copied().collect();
    let mut pending = Vec::new();
    for boundary in workspace_boundaries {
        let Some(name) = boundary.manifest.metadata.name.as_deref() else {
            continue;
        };
        let name = normalize_distribution_name(name);
        if !declared.contains(&name) || !manifest.uv.is_local_source(&name) {
            continue;
        }
        let Some(member_path) = member_path(manifest, boundary.manifest) else {
            continue;
        };
        let provided = member_modules(&member_path, &context.sources.files);
        pending.push((name, member_path, provided));
    }

    loop {
        let (now_used, rest): (Vec<_>, Vec<_>) =
            pending.into_iter().partition(|(name, _, provided)| {
                used.contains(name) || modules.iter().any(|module| provided.contains(*module))
            });
        pending = rest;
        if now_used.is_empty() {
            break;
        }
        for (name, member_path, _) in now_used {
            if !member_path.is_empty() {
                modules.extend(member_imports(context.resolution, &member_path));
            }
            used.insert(name);
        }
    }
}

/// Non-stdlib modules imported by files under a non-root member tree.
fn member_imports<'a>(
    resolution: &'a ResolutionIndex,
    member_path: &str,
) -> impl Iterator<Item = &'a str> + use<'a> {
    let prefix = format!("{member_path}/");
    resolution
        .imports
        .iter()
        .filter(move |import| {
            import.origin != ModuleOrigin::Stdlib && import.file.starts_with(&prefix)
        })
        .map(|import| import.full_module.as_str())
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

/// Dotted module names the files under `member_path` provide, imported from
/// the member root or from a `src` directory (`providers/x/src/airflow/models.py`
/// gives `airflow` and `airflow.models`).
fn member_modules(member_path: &str, files: &[DiscoveredFile]) -> HashSet<String> {
    let mut modules = HashSet::new();
    for file in files {
        let relative = if member_path.is_empty() {
            Some(file.path.as_str())
        } else {
            file.path
                .strip_prefix(member_path)
                .and_then(|rest| rest.strip_prefix('/'))
        };
        let Some(relative) = relative else {
            continue;
        };
        let Some(stem) = relative
            .strip_suffix(".py")
            .or_else(|| relative.strip_suffix(".pyi"))
        else {
            continue;
        };
        let mut parts: Vec<&str> = stem.split('/').collect();
        if parts.last() == Some(&"__init__") {
            parts.pop();
        }
        let starts = std::iter::once(0).chain(
            parts
                .iter()
                .enumerate()
                .filter(|(_, part)| **part == "src")
                .map(|(index, _)| index + 1),
        );
        for start in starts {
            for end in start + 1..=parts.len() {
                modules.insert(parts[start..end].join("."));
            }
        }
    }
    modules
}

/// Treat `pytest11` plugins installed in the venv as used whenever pytest is,
/// since pytest loads them without any import or config reference.
pub(super) fn mark_pytest_plugin_distributions(
    resolution: &ResolutionIndex,
    used: &mut IndexSet<String>,
) {
    if !used.contains("pytest") {
        return;
    }
    for distribution in &resolution.pytest_plugin_distributions {
        used.insert(distribution.clone());
    }
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
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::{FileNode, ProjectGraph};
    use crate::manifest::{LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata};
    use crate::parser::ImportContext;
    use crate::reachability::ReachabilityReport;
    use crate::resolver::{ResolveConfidence, ResolvedImport};

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
            imports: vec![ResolvedImport {
                import_root: "yaml".to_owned(),
                full_module: "yaml".to_owned(),
                file: "src/app.py".to_owned(),
                workspace_member: None,
                line: 1,
                context: ImportContext::Runtime,
                optional: false,
                platform_guarded: false,
                origin: ModuleOrigin::ThirdParty,
                distribution: Some("pyyaml".to_owned()),
                confidence: ResolveConfidence::Certain,
            }],
            warnings: Vec::new(),
            transitive: LockfileGraph::default(),
            binary_resolutions: BTreeMap::new(),
            pytest_plugin_distributions: std::collections::BTreeSet::new(),
        };
        let sources = crate::sources::DiscoveredSources {
            root: graph.root.clone(),
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Src,
                package_root: "src".to_owned(),
                packages: Vec::new(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
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
            },
            &PluginHints {
                contributions: Vec::new(),
                config_binary_usages: Vec::new(),
                config_used_distributions: Vec::new(),
                config_module_refs: Vec::new(),
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
        let file = |path: &str| DiscoveredFile {
            path: path.to_owned(),
            kind: crate::sources::FileKind::Python,
            context: crate::sources::FileContext::Runtime,
        };
        let files = [
            file("airflow-core/src/airflow/__init__.py"),
            file("airflow-core/src/airflow/models/dag.py"),
            file("airflow-core/tests/utils.py"),
            file("task-sdk/src/airflow/sdk/__init__.py"),
        ];
        let modules = member_modules("airflow-core", &files);
        assert!(modules.contains("airflow"));
        assert!(modules.contains("airflow.models.dag"));
        assert!(modules.contains("tests.utils"));
        assert!(!modules.contains("utils"));
        assert!(!modules.contains("models"));
        assert!(!modules.contains("airflow.sdk"));
        assert!(!modules.contains(""));
    }

    #[test]
    fn member_imports_stay_in_member_tree() {
        let import = |file: &str, module: &str, origin: ModuleOrigin| ResolvedImport {
            import_root: import_root(module).to_owned(),
            full_module: module.to_owned(),
            file: file.to_owned(),
            workspace_member: None,
            line: 1,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            origin,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        };
        let resolution = ResolutionIndex {
            imports: vec![
                import(
                    "airflow-core/src/airflow/models/dag.py",
                    "airflow.sdk",
                    ModuleOrigin::FirstParty,
                ),
                import(
                    "airflow-core/src/airflow/models/dag.py",
                    "airflow.sdk.definitions.dag",
                    ModuleOrigin::ThirdParty,
                ),
                import(
                    "airflow-core/src/airflow/models/dag.py",
                    "os",
                    ModuleOrigin::Stdlib,
                ),
                import(
                    "airflow-core-extra/src/extra.py",
                    "extra.api",
                    ModuleOrigin::FirstParty,
                ),
            ],
            warnings: Vec::new(),
            transitive: LockfileGraph::default(),
            binary_resolutions: BTreeMap::new(),
            pytest_plugin_distributions: std::collections::BTreeSet::new(),
        };
        let modules: Vec<&str> = member_imports(&resolution, "airflow-core").collect();
        assert_eq!(modules, ["airflow.sdk", "airflow.sdk.definitions.dag"]);
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
                })
                .collect(),
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings {
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
    /// inside `core`. `extra` is imported only by an unreachable file, `stray`
    /// is not declared, and `plain` has no workspace source.
    #[test]
    fn workspace_sources_are_used_through_member_trees() {
        let root = std::env::temp_dir().join("ws");
        let manifest = manifest_at(
            root.clone(),
            "root",
            &["core", "sdk", "extra", "plain"],
            &["core", "sdk", "extra", "stray"],
        );
        let members = ["core", "sdk", "extra", "stray", "plain"]
            .map(|name| (name, manifest_at(root.join(name), name, &[], &[])));
        let boundaries: Vec<WorkspaceDependencyBoundary<'_>> = members
            .iter()
            .map(|(name, member)| WorkspaceDependencyBoundary {
                member_id: name,
                manifest: member,
            })
            .collect();
        let import = |file: &str, module: &str| ResolvedImport {
            import_root: import_root(module).to_owned(),
            full_module: module.to_owned(),
            file: file.to_owned(),
            workspace_member: None,
            line: 1,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            origin: ModuleOrigin::FirstParty,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        };
        let resolution = ResolutionIndex {
            imports: vec![
                import("src/app.py", "corepkg.models"),
                import("src/app.py", "straypkg"),
                import("src/app.py", "plainpkg"),
                import("scripts/old.py", "extrapkg"),
                import("core/src/corepkg/models.py", "sdkpkg"),
            ],
            ..ResolutionIndex::default()
        };
        let file = |path: &str| DiscoveredFile {
            path: path.to_owned(),
            kind: crate::sources::FileKind::Python,
            context: crate::sources::FileContext::Runtime,
        };
        let sources = crate::sources::DiscoveredSources {
            root: manifest.root.clone(),
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Src,
                package_root: "src".to_owned(),
                packages: Vec::new(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: members
                .iter()
                .map(|(name, _)| file(&format!("{name}/src/{name}pkg/__init__.py")))
                .chain([file("core/src/corepkg/models.py")])
                .collect(),
            warnings: Vec::new(),
        };
        let graph = ProjectGraph::new(manifest.root.clone());
        let mut used = IndexSet::new();
        mark_workspace_source_distributions(
            &manifest,
            &RuleContext {
                resolution: &resolution,
                reachability: &ReachabilityReport::default(),
                graph: &graph,
                sources: &sources,
                parse: &crate::parser::ParseSummary::default(),
            },
            &HashSet::from(["src/app.py"]),
            &boundaries,
            &mut used,
        );
        let mut used: Vec<String> = used.into_iter().collect();
        used.sort();
        assert_eq!(used, ["core", "sdk"]);
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
            imports: vec![ResolvedImport {
                import_root: "streamlit".to_owned(),
                full_module: "streamlit.web".to_owned(),
                file: "e2e/app.py".to_owned(),
                workspace_member: None,
                line: 1,
                context: ImportContext::Runtime,
                optional: false,
                platform_guarded: false,
                origin: ModuleOrigin::FirstParty,
                distribution: None,
                confidence: ResolveConfidence::Certain,
            }],
            ..ResolutionIndex::default()
        };
        let sources = crate::sources::DiscoveredSources {
            root: manifest.root.clone(),
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Src,
                package_root: "lib".to_owned(),
                packages: vec!["streamlit".to_owned()],
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let graph = ProjectGraph::new(manifest.root.clone());
        let mut used = IndexSet::new();
        mark_workspace_source_distributions(
            &manifest,
            &RuleContext {
                resolution: &resolution,
                reachability: &ReachabilityReport::default(),
                graph: &graph,
                sources: &sources,
                parse: &crate::parser::ParseSummary::default(),
            },
            &HashSet::from(["e2e/app.py"]),
            &[],
            &mut used,
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
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Flat,
                package_root: String::new(),
                packages: Vec::new(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let graph = ProjectGraph::new(manifest.root.clone());
        let mut used = IndexSet::new();
        mark_workspace_source_distributions(
            &manifest,
            &RuleContext {
                resolution: &ResolutionIndex::default(),
                reachability: &ReachabilityReport::default(),
                graph: &graph,
                sources: &sources,
                parse: &crate::parser::ParseSummary::default(),
            },
            &HashSet::new(),
            &[],
            &mut used,
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
        };
        let mut report = ReachabilityReport::default();
        report.unreachable = vec![
            orphan("public.py", ProjectMode::Library, Confidence::Maybe),
            orphan(
                "outside_wheel.py",
                ProjectMode::Library,
                Confidence::Certain,
            ),
            orphan("app_orphan.py", ProjectMode::App, Confidence::Maybe),
        ];
        let reachable = HashSet::from(["main.py"]);

        let usage = usage_paths(&reachable, &report);

        assert_eq!(usage, HashSet::from(["main.py", "public.py"]));
    }
}
