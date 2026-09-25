//! Import resolution orchestration.

use std::collections::BTreeMap;

use crate::config::{ChokkinConfig, ResolvedWorkspaceMember, TargetVersion};
use crate::graph::ModuleOrigin;
use crate::manifest::LoadedManifest;
use crate::parser::{ImportContext, ParseSummary};
use crate::plugins::ModuleReference;
use crate::sources::DiscoveredSources;

use super::first_party::{is_first_party_import, is_workspace_import};
use super::maps::{ImportMap, build_binary_map};
use super::stdlib::is_stdlib_import;
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
#[allow(clippy::too_many_arguments)]
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

/// [`resolve_imports`] with per-file target versions for PEP 723 scripts.
///
/// A script's `requires-python` decides which modules are stdlib for the
/// imports in that file; every other file uses the project target.
#[must_use]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn resolve_imports_with_script_targets(
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    parse: &ParseSummary,
    plugin_refs: &[ModuleReference],
    workspace_members: &[ResolvedWorkspaceMember],
    script_targets: &BTreeMap<String, TargetVersion>,
) -> ResolutionIndex {
    let target = config
        .target_version
        .as_ref()
        .map_or_else(TargetVersion::default_py311, Clone::clone);

    let import_map = ImportMap::build(config);
    let mut warnings = Vec::new();
    let venv_index = load_venv_index(&manifest.root, &mut warnings);
    let binary_resolutions = build_binary_map(config, &venv_index);
    let mut imports = Vec::new();
    let mut root_cache: RootCache = BTreeMap::new();

    for module in &parse.modules {
        let file_target = script_targets.get(&module.path).unwrap_or(&target);
        for import in &module.imports {
            if import.module.is_empty() {
                continue;
            }
            imports.push(resolve_import_site(
                &import.module,
                &module.path,
                import.line,
                import.context,
                import.optional,
                import.platform_guarded,
                file_target,
                sources,
                manifest,
                config,
                workspace_members,
                &import_map,
                &venv_index.imports,
                &mut warnings,
                &mut root_cache,
            ));
        }
        for dynamic in &module.dynamic_imports {
            imports.push(resolve_import_site(
                &dynamic.module,
                &module.path,
                dynamic.line,
                ImportContext::Runtime,
                false,
                false,
                file_target,
                sources,
                manifest,
                config,
                workspace_members,
                &import_map,
                &venv_index.imports,
                &mut warnings,
                &mut root_cache,
            ));
        }
    }

    for reference in plugin_refs {
        imports.push(resolve_import_site(
            &reference.module,
            &reference.origin.file,
            reference.origin.line.unwrap_or(0),
            ImportContext::Runtime,
            false,
            false,
            &target,
            sources,
            manifest,
            config,
            workspace_members,
            &import_map,
            &venv_index.imports,
            &mut warnings,
            &mut root_cache,
        ));
    }

    ResolutionIndex {
        imports,
        warnings,
        transitive: transitive_index(manifest),
        binary_resolutions,
    }
}

fn transitive_index(manifest: &LoadedManifest) -> TransitiveIndex {
    TransitiveIndex {
        edges: manifest.lockfile.edges.clone(),
    }
}

/// Keyed by (target version, import root): stdlib membership depends on the
/// target, which differs between PEP 723 scripts.
type RootCache = BTreeMap<(String, String), RootResolution>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootResolution {
    origin: ModuleOrigin,
    distribution: Option<String>,
    confidence: ResolveConfidence,
}

#[allow(clippy::too_many_arguments)]
fn resolve_import_site(
    full_module: &str,
    file: &str,
    line: u32,
    context: ImportContext,
    optional: bool,
    platform_guarded: bool,
    target: &TargetVersion,
    sources: &DiscoveredSources,
    manifest: &LoadedManifest,
    config: &ChokkinConfig,
    workspace_members: &[ResolvedWorkspaceMember],
    import_map: &ImportMap,
    venv_imports: &BTreeMap<String, Vec<String>>,
    warnings: &mut Vec<ResolveWarning>,
    root_cache: &mut RootCache,
) -> ResolvedImport {
    let root_name = import_root(full_module).to_owned();
    let core = root_cache
        .entry((target.as_str().to_owned(), root_name.clone()))
        .or_insert_with(|| {
            resolve_import_root(
                &root_name,
                target,
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
        workspace_member: workspace_member_for_file(file, workspace_members),
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
    target: &TargetVersion,
    sources: &DiscoveredSources,
    manifest: &LoadedManifest,
    config: &ChokkinConfig,
    workspace_members: &[ResolvedWorkspaceMember],
    import_map: &ImportMap,
    venv_imports: &BTreeMap<String, Vec<String>>,
    warnings: &mut Vec<ResolveWarning>,
) -> RootResolution {
    if is_stdlib_import(root_name, target) {
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

    RootResolution {
        origin: ModuleOrigin::Unknown,
        distribution: None,
        confidence: ResolveConfidence::Maybe,
    }
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
        warnings.push(ResolveWarning::AmbiguousImport {
            import: root_name.to_owned(),
            candidates: candidates.to_vec(),
        });
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
