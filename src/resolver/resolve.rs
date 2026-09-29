//! Import resolution orchestration.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::{ChokkinConfig, ResolvedWorkspaceMember, TargetVersion};
use crate::graph::ModuleOrigin;
use crate::manifest::{LoadedManifest, normalize_distribution_name};
use crate::parser::{ImportContext, ParseSummary};
use crate::plugins::ModuleReference;
use crate::sources::{DiscoveredFile, DiscoveredSources};

use super::first_party::{is_first_party_import, is_workspace_import, path_source_imports};
use super::maps::{ImportMap, build_binary_map};
use super::pytest_path::PytestImportPaths;
use super::stdlib::StdlibRange;
use super::types::{
    ResolutionIndex, ResolveConfidence, ResolveWarning, ResolvedImport, TransitiveIndex,
    import_root,
};
use super::venv::load_venv_index;

/// Resolve parsed imports and plugin module references to origins and distributions.
///
/// `workspace_members` marks cross-member imports as first-party so workspace
/// packages do not become false missing-dependency findings.
#[must_use]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn resolve_imports(
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    parse: &ParseSummary,
    plugin_refs: &[ModuleReference],
    workspace_members: &[ResolvedWorkspaceMember],
) -> ResolutionIndex {
    resolve_imports_with_script_targets(
        config,
        manifest,
        sources,
        parse,
        plugin_refs,
        workspace_members,
        &BTreeMap::new(),
    )
}

/// [`resolve_imports`] with per-file stdlib ranges for PEP 723 scripts.
///
/// A script's `requires-python` decides which modules are stdlib for the
/// imports in that file; every other file uses the project range.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn resolve_imports_with_script_targets(
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    parse: &ParseSummary,
    plugin_refs: &[ModuleReference],
    workspace_members: &[ResolvedWorkspaceMember],
    script_targets: &BTreeMap<String, StdlibRange>,
) -> ResolutionIndex {
    resolve_imports_for_analysis(
        config,
        manifest,
        sources,
        parse,
        plugin_refs,
        workspace_members,
        script_targets,
        &ScopedDeclarations::default(),
    )
}

/// Distributions declared outside the root manifest, for the files they cover.
#[derive(Debug, Default)]
pub struct ScopedDeclarations {
    /// PEP 723 script path → normalized names its block declares.
    pub scripts: BTreeMap<String, BTreeSet<String>>,
    /// Workspace member id → normalized names its manifest declares or locks.
    pub members: BTreeMap<String, BTreeSet<String>>,
}

/// [`resolve_imports_with_script_targets`] that also resolves an unmapped
/// root through the script block or member manifest owning the file.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn resolve_imports_for_analysis(
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    parse: &ParseSummary,
    plugin_refs: &[ModuleReference],
    workspace_members: &[ResolvedWorkspaceMember],
    script_targets: &BTreeMap<String, StdlibRange>,
    scoped: &ScopedDeclarations,
) -> ResolutionIndex {
    let target = config
        .target_version
        .as_ref()
        .map_or_else(TargetVersion::default_py311, Clone::clone);
    let stdlib = StdlibRange::new(&target, manifest.metadata.requires_python.as_deref());

    let import_map = ImportMap::build(config)
        .with_local_sources(path_source_imports(&manifest.root.path, &manifest.uv));
    let mut warnings = Vec::new();
    let venv_index = load_venv_index(&manifest.root, &mut warnings);
    let binary_resolutions = build_binary_map(config, &venv_index);
    let mut imports = Vec::new();
    let mut root_cache: RootCache = BTreeMap::new();
    let pytest_paths = PytestImportPaths::build(sources);

    for module in &parse.modules {
        let file_stdlib = script_targets.get(&module.path).copied().unwrap_or(stdlib);
        for import in &module.imports {
            if import.module.is_empty() {
                continue;
            }
            let imported = import.name.as_ref().map_or_else(
                || import.module.clone(),
                |name| format!("{}.{name}", import.module),
            );
            imports.push(resolve_import_site(
                &import.module,
                &imported,
                &module.path,
                import.line,
                import.context,
                import.optional,
                import.platform_guarded,
                file_stdlib,
                sources,
                manifest,
                config,
                workspace_members,
                &import_map,
                &venv_index.imports,
                scoped,
                &pytest_paths,
                &mut warnings,
                &mut root_cache,
            ));
        }
        for dynamic in &module.dynamic_imports {
            imports.push(resolve_import_site(
                &dynamic.module,
                &dynamic.module,
                &module.path,
                dynamic.line,
                ImportContext::Runtime,
                false,
                false,
                file_stdlib,
                sources,
                manifest,
                config,
                workspace_members,
                &import_map,
                &venv_index.imports,
                scoped,
                &pytest_paths,
                &mut warnings,
                &mut root_cache,
            ));
        }
    }

    for reference in plugin_refs {
        imports.push(resolve_import_site(
            &reference.module,
            &reference.module,
            &reference.origin.file,
            reference.origin.line.unwrap_or(0),
            ImportContext::Runtime,
            false,
            false,
            stdlib,
            sources,
            manifest,
            config,
            workspace_members,
            &import_map,
            &venv_index.imports,
            scoped,
            &pytest_paths,
            &mut warnings,
            &mut root_cache,
        ));
    }

    ResolutionIndex {
        imports,
        warnings,
        transitive: transitive_index(manifest),
        binary_resolutions,
        pytest_plugin_distributions: venv_index.pytest_plugins,
    }
}

fn transitive_index(manifest: &LoadedManifest) -> TransitiveIndex {
    TransitiveIndex {
        edges: manifest.lockfile.edges.clone(),
    }
}

/// Keyed by (stdlib range, import root): stdlib membership depends on the
/// range, which differs between PEP 723 scripts.
type RootCache = BTreeMap<(StdlibRange, String), RootResolution>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootResolution {
    origin: ModuleOrigin,
    distribution: Option<String>,
    confidence: ResolveConfidence,
}

#[allow(clippy::too_many_arguments)]
fn resolve_import_site(
    full_module: &str,
    imported: &str,
    file: &str,
    line: u32,
    context: ImportContext,
    optional: bool,
    platform_guarded: bool,
    stdlib: StdlibRange,
    sources: &DiscoveredSources,
    manifest: &LoadedManifest,
    config: &ChokkinConfig,
    workspace_members: &[ResolvedWorkspaceMember],
    import_map: &ImportMap,
    venv_imports: &BTreeMap<String, Vec<String>>,
    scoped: &ScopedDeclarations,
    pytest_paths: &PytestImportPaths,
    warnings: &mut Vec<ResolveWarning>,
    root_cache: &mut RootCache,
) -> ResolvedImport {
    let root_name = import_root(full_module).to_owned();
    // pytest puts the test's basedir on `sys.path` ahead of site-packages, so
    // a local module there shadows any distribution of the same name.
    let pytest_local = !stdlib.contains(&root_name) && pytest_paths.provides_root(file, &root_name);
    let core = if pytest_local {
        RootResolution {
            origin: ModuleOrigin::FirstParty,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        }
    } else {
        let core = root_cache
            .entry((stdlib, root_name.clone()))
            .or_insert_with(|| {
                resolve_import_root(
                    &root_name,
                    stdlib,
                    sources,
                    manifest,
                    config,
                    workspace_members,
                    import_map,
                    venv_imports,
                    warnings,
                )
            })
            .clone();
        match (core.origin, import_map.namespace_candidates(imported)) {
            (ModuleOrigin::Stdlib, _) | (_, None) => core,
            // A first-party root may share its namespace with a distribution
            // (`poetry` and `poetry.core`); the local tree wins when it has the
            // module itself.
            (ModuleOrigin::FirstParty, Some((module, ..)))
                if has_local_module(module, &sources.files, workspace_members) =>
            {
                core
            },
            (_, Some((_, distributions, confidence))) => root_resolution_from_candidates(
                &root_name,
                &distributions,
                Some(confidence),
                warnings,
            ),
        }
    };

    let workspace_member = workspace_member_for_file(file, workspace_members);
    let core = if core.origin == ModuleOrigin::Unknown {
        scoped_declaration(&root_name, file, workspace_member.as_deref(), scoped).unwrap_or(core)
    } else {
        core
    };

    if core.origin == ModuleOrigin::Unknown {
        warnings.push(ResolveWarning::UnresolvedImport {
            import: root_name.clone(),
            file: file.to_owned(),
            line,
        });
    }

    ResolvedImport {
        import_root: root_name,
        full_module: full_module.to_owned(),
        file: file.to_owned(),
        workspace_member,
        line,
        context,
        optional,
        platform_guarded,
        origin: core.origin,
        distribution: core.distribution,
        confidence: core.confidence,
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn resolve_import_root(
    root_name: &str,
    stdlib: StdlibRange,
    sources: &DiscoveredSources,
    manifest: &LoadedManifest,
    config: &ChokkinConfig,
    workspace_members: &[ResolvedWorkspaceMember],
    import_map: &ImportMap,
    venv_imports: &BTreeMap<String, Vec<String>>,
    warnings: &mut Vec<ResolveWarning>,
) -> RootResolution {
    if stdlib.contains(root_name) {
        return RootResolution {
            origin: ModuleOrigin::Stdlib,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        };
    }

    if is_first_party_import(root_name, &sources.layout, &manifest.metadata) {
        return RootResolution {
            origin: ModuleOrigin::FirstParty,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        };
    }

    if is_workspace_import(
        root_name,
        workspace_members,
        manifest.uv_workspace.as_ref(),
        config,
    ) {
        return RootResolution {
            origin: ModuleOrigin::FirstParty,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        };
    }

    if let Some(distributions) = venv_imports.get(root_name) {
        return root_resolution_from_candidates(root_name, distributions, None, warnings);
    }

    if let Some((distributions, confidence)) = import_map.candidates(root_name) {
        return root_resolution_from_candidates(
            root_name,
            &distributions,
            Some(confidence),
            warnings,
        );
    }

    // No map names the root. A declared or locked distribution whose
    // normalized name matches (`openai`, or `Foo_Bar` for `import foo_bar`)
    // is the only evidence left; the spelling alone is not, or a local module
    // like `e2e_config` would pass as a missing `e2e-config` (#361).
    let distribution = normalize_distribution_name(root_name);
    if manifest
        .dependencies
        .iter()
        .any(|dep| normalize_distribution_name(&dep.name) == distribution)
        || manifest.lockfile.edges.contains_key(&distribution)
    {
        return RootResolution {
            origin: ModuleOrigin::ThirdParty,
            distribution: Some(distribution),
            confidence: ResolveConfidence::Likely,
        };
    }

    RootResolution {
        origin: ModuleOrigin::Unknown,
        distribution: None,
        confidence: ResolveConfidence::Maybe,
    }
}

/// Per-file counterpart of the declared-name step in [`resolve_import_root`]:
/// a script block or member manifest declares for its own files only, so it
/// cannot go through the per-root cache.
fn scoped_declaration(
    root_name: &str,
    file: &str,
    workspace_member: Option<&str>,
    scoped: &ScopedDeclarations,
) -> Option<RootResolution> {
    let distribution = normalize_distribution_name(root_name);
    scoped
        .scripts
        .get(file)
        .into_iter()
        .chain(workspace_member.and_then(|member| scoped.members.get(member)))
        .any(|names| names.contains(&distribution))
        .then_some(RootResolution {
            origin: ModuleOrigin::ThirdParty,
            distribution: Some(distribution),
            confidence: ResolveConfidence::Likely,
        })
}

/// Whether any discovered file is `module` or lies under it, from the project
/// root or a workspace member, in flat or `src/` layout. Deeper matches such as
/// `tests/fixtures/vendor/poetry/core/` are not importable as `poetry.core`.
fn has_local_module(
    module: &str,
    files: &[DiscoveredFile],
    workspace_members: &[ResolvedWorkspaceMember],
) -> bool {
    let path = module.replace('.', "/");
    let roots: Vec<String> = ["", "src/"]
        .into_iter()
        .map(str::to_owned)
        .chain(
            workspace_members
                .iter()
                .flat_map(|member| [format!("{}/", member.path), format!("{}/src/", member.path)]),
        )
        .collect();
    files.iter().any(|file| {
        let file = file.path.replace('\\', "/");
        roots.iter().any(|root| {
            file.strip_prefix(root.as_str()).is_some_and(|rest| {
                rest.strip_prefix(path.as_str())
                    .is_some_and(|tail| matches!(tail, ".py" | ".pyi") || tail.starts_with('/'))
            })
        })
    })
}

fn workspace_member_for_file(
    file: &str,
    workspace_members: &[ResolvedWorkspaceMember],
) -> Option<String> {
    let normalized = file.replace('\\', "/");
    workspace_members
        .iter()
        .filter(|member| {
            normalized == member.path || normalized.starts_with(&format!("{}/", member.path))
        })
        .max_by_key(|member| member.path.len())
        .map(|member| member.id.clone())
}

fn root_resolution_from_candidates(
    root_name: &str,
    candidates: &[String],
    confidence_override: Option<ResolveConfidence>,
    warnings: &mut Vec<ResolveWarning>,
) -> RootResolution {
    if candidates.len() > 1 {
        // Namespace matches bypass the root cache and repeat per import site.
        let warning = ResolveWarning::AmbiguousImport {
            import: root_name.to_owned(),
            candidates: candidates.to_vec(),
        };
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }
    let confidence = match confidence_override {
        Some(value) => value,
        None => {
            if candidates.len() == 1 {
                ResolveConfidence::Certain
            } else {
                ResolveConfidence::Maybe
            }
        },
    };
    RootResolution {
        origin: ModuleOrigin::ThirdParty,
        distribution: candidates.first().cloned(),
        confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(candidates: &[&str]) -> (RootResolution, Vec<ResolveWarning>) {
        let candidates: Vec<String> = candidates.iter().map(|c| (*c).to_owned()).collect();
        let mut warnings = Vec::new();
        let resolution = root_resolution_from_candidates("yaml", &candidates, None, &mut warnings);
        (resolution, warnings)
    }

    #[test]
    fn single_candidate_is_certain_and_not_ambiguous() {
        let (resolution, warnings) = resolve(&["pyyaml"]);
        assert_eq!(resolution.distribution.as_deref(), Some("pyyaml"));
        assert_eq!(resolution.confidence, ResolveConfidence::Certain);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn several_candidates_are_maybe_and_ambiguous() {
        let (resolution, warnings) = resolve(&["pyyaml", "ruamel-yaml"]);
        assert_eq!(resolution.distribution.as_deref(), Some("pyyaml"));
        assert_eq!(resolution.confidence, ResolveConfidence::Maybe);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }
}
