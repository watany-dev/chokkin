//! Import resolution orchestration.

use crate::config::{TargetVersion, YokeiConfig};
use crate::discovery::ProjectRoot;
use crate::graph::ModuleOrigin;
use crate::manifest::LoadedManifest;
use crate::parser::{ImportContext, ParseSummary};
use crate::plugins::ModuleReference;
use crate::sources::DiscoveredSources;

use super::error::ResolveError;
use super::first_party::{is_first_party_import, is_workspace_import};
use super::maps::{ImportMap, build_binary_map};
use super::stdlib::is_stdlib_import;
use super::transitive::build_transitive_index;
use super::types::{
    ResolutionIndex, ResolveConfidence, ResolveWarning, ResolvedImport, import_root,
};
use super::venv::load_venv_index;

/// Resolve parsed imports and plugin module references to origins and distributions.
///
/// # Errors
///
/// Returns [`ResolveError`] only for internal invariant failures (v0.1).
#[allow(clippy::too_many_arguments)]
pub fn resolve_imports(
    root: &ProjectRoot,
    config: &YokeiConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    parse: &ParseSummary,
    plugin_refs: &[ModuleReference],
) -> Result<ResolutionIndex, ResolveError> {
    let _ = root;
    let target = config
        .target_version
        .as_ref()
        .map_or_else(TargetVersion::default_py311, Clone::clone);

    let import_map = ImportMap::build(config);
    let mut warnings = Vec::new();
    let venv_index = load_venv_index(&manifest.root, &mut warnings);
    let binary_resolutions = build_binary_map(config);
    let mut imports = Vec::new();

    for module in &parse.modules {
        for import in &module.imports {
            if import.module.is_empty() {
                continue;
            }
            imports.push(resolve_module(
                &import.module,
                &module.path,
                import.line,
                import.context,
                &target,
                sources,
                manifest,
                config,
                &import_map,
                &venv_index.imports,
                &mut warnings,
            ));
        }
        for dynamic in &module.dynamic_imports {
            imports.push(resolve_module(
                &dynamic.module,
                &module.path,
                dynamic.line,
                ImportContext::Runtime,
                &target,
                sources,
                manifest,
                config,
                &import_map,
                &venv_index.imports,
                &mut warnings,
            ));
        }
    }

    for reference in plugin_refs {
        imports.push(resolve_module(
            &reference.module,
            &reference.origin.file,
            reference.origin.line.unwrap_or(0),
            ImportContext::Runtime,
            &target,
            sources,
            manifest,
            config,
            &import_map,
            &venv_index.imports,
            &mut warnings,
        ));
    }

    Ok(ResolutionIndex {
        imports,
        warnings,
        transitive: build_transitive_index(manifest),
        binary_resolutions,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn resolve_module(
    full_module: &str,
    file: &str,
    line: u32,
    context: ImportContext,
    target: &TargetVersion,
    sources: &DiscoveredSources,
    manifest: &LoadedManifest,
    config: &YokeiConfig,
    import_map: &ImportMap,
    venv_imports: &std::collections::BTreeMap<String, Vec<String>>,
    warnings: &mut Vec<ResolveWarning>,
) -> ResolvedImport {
    let root_name = import_root(full_module).to_owned();

    if is_stdlib_import(&root_name, target) {
        return ResolvedImport {
            import_root: root_name,
            full_module: full_module.to_owned(),
            file: file.to_owned(),
            line,
            context,
            origin: ModuleOrigin::Stdlib,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        };
    }

    if is_first_party_import(&root_name, &sources.layout, &manifest.metadata) {
        return ResolvedImport {
            import_root: root_name,
            full_module: full_module.to_owned(),
            file: file.to_owned(),
            line,
            context,
            origin: ModuleOrigin::FirstParty,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        };
    }

    if is_workspace_import(&root_name, manifest.uv_workspace.as_ref(), config) {
        return ResolvedImport {
            import_root: root_name,
            full_module: full_module.to_owned(),
            file: file.to_owned(),
            line,
            context,
            origin: ModuleOrigin::FirstParty,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        };
    }

    if let Some(distributions) = venv_imports.get(&root_name) {
        return resolve_from_candidates(
            &root_name,
            full_module,
            file,
            line,
            context,
            distributions,
            None,
            warnings,
        );
    }

    let map_candidates = import_map.candidates(&root_name);
    if !map_candidates.is_empty() {
        let distributions: Vec<String> = map_candidates
            .iter()
            .map(|candidate| candidate.distribution.clone())
            .collect();
        let confidence = map_candidates
            .iter()
            .map(|candidate| candidate.confidence)
            .max_by_key(|confidence| confidence_rank(*confidence))
            .unwrap_or(ResolveConfidence::Maybe);
        return resolve_from_candidates(
            &root_name,
            full_module,
            file,
            line,
            context,
            &distributions,
            Some(confidence),
            warnings,
        );
    }
    warnings.push(ResolveWarning::UnresolvedImport {
        import: root_name.clone(),
        file: file.to_owned(),
        line,
    });
    ResolvedImport {
        import_root: root_name,
        full_module: full_module.to_owned(),
        file: file.to_owned(),
        line,
        context,
        origin: ModuleOrigin::Unknown,
        distribution: None,
        confidence: ResolveConfidence::Maybe,
    }
}

fn confidence_rank(confidence: ResolveConfidence) -> u8 {
    match confidence {
        ResolveConfidence::Certain => 2,
        ResolveConfidence::Likely => 1,
        ResolveConfidence::Maybe => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_from_candidates(
    root_name: &str,
    full_module: &str,
    file: &str,
    line: u32,
    context: ImportContext,
    candidates: &[String],
    confidence_override: Option<ResolveConfidence>,
    warnings: &mut Vec<ResolveWarning>,
) -> ResolvedImport {
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
    ResolvedImport {
        import_root: root_name.to_owned(),
        full_module: full_module.to_owned(),
        file: file.to_owned(),
        line,
        context,
        origin: ModuleOrigin::ThirdParty,
        distribution: candidates.first().cloned(),
        confidence,
    }
}
