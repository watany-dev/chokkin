//! Build the set of used distributions from imports, plugins, and binaries.

use std::collections::{BTreeMap, HashSet};

use indexmap::IndexSet;

use crate::graph::{ModuleOrigin, ProjectGraph};
use crate::manifest::{LoadedManifest, normalize_distribution_name};
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

/// `workspace = true` dependencies resolve as first-party imports, so they
/// carry no distribution; match them back by normalized import name, or by
/// the member tree that holds the imported module (`airflow` from
/// `airflow-core/src/airflow` uses `apache-airflow-core`). The manifest's own
/// entry point targets count as such imports too.
pub(super) fn mark_workspace_source_distributions(
    manifest: &LoadedManifest,
    resolution: &ResolutionIndex,
    reachable: &HashSet<&str>,
    files: &[DiscoveredFile],
    workspace_boundaries: &[WorkspaceDependencyBoundary<'_>],
    used: &mut IndexSet<String>,
) {
    let first_party: Vec<&str> = resolution
        .imports
        .iter()
        .filter(|import| {
            import.origin == ModuleOrigin::FirstParty && reachable.contains(import.file.as_str())
        })
        .map(|import| import.full_module.as_str())
        .collect();
    for module in &first_party {
        let name = normalize_distribution_name(import_root(module));
        if manifest.uv.is_workspace_source(&name) {
            used.insert(name);
        }
    }

    let declared: HashSet<String> = manifest
        .dependencies
        .iter()
        .map(|dep| normalize_distribution_name(&dep.name))
        .collect();
    let entry_modules = manifest
        .entry_points
        .iter()
        .filter_map(|entry| entry.target.split(':').next())
        .map(str::trim);
    let modules: HashSet<&str> = first_party.iter().copied().chain(entry_modules).collect();
    for boundary in workspace_boundaries {
        let Some(name) = boundary.manifest.metadata.name.as_deref() else {
            continue;
        };
        let name = normalize_distribution_name(name);
        if used.contains(&name)
            || !declared.contains(&name)
            || !manifest.uv.is_workspace_source(&name)
        {
            continue;
        }
        let Some(member_path) = member_path(manifest, boundary.manifest) else {
            continue;
        };
        let provided = member_modules(&member_path, files);
        if modules.iter().any(|module| provided.contains(*module)) {
            used.insert(name);
        }
    }
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
    use crate::resolver::{ResolveConfidence, ResolvedImport, TransitiveIndex};

    #[test]
    fn detects_lockfile_from_manifest_sources() {
        let manifest = LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
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
            start: std::env::temp_dir(),
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
            transitive: TransitiveIndex::default(),
            binary_resolutions: BTreeMap::new(),
            pytest_plugin_distributions: std::collections::BTreeSet::new(),
        };
        let sources = crate::sources::DiscoveredSources {
            root: graph.root.clone(),
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Src,
                packages: Vec::new(),
                inferred_globs: Vec::new(),
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
                start: std::env::temp_dir(),
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
    }
}
