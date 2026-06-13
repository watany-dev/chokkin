//! Build entry roots from config, manifest, plugins, and auto-detection.

use std::collections::BTreeSet;

use crate::config::{EntrySpec, YokeiConfig};
use crate::manifest::LoadedManifest;
use crate::plugins::{PluginHints, parse_module_symbol, parse_uvicorn_script_target};
use crate::sources::{DiscoveredSources, FileContext, assign_file_context};

use super::auto::detect_auto_entries;
use super::error::EntryError;
use super::merge::merge_entry_candidates;
use super::mode::resolve_project_mode;
use super::module::resolve_module_to_path;
use super::types::{EntryCandidate, EntryOrigin, EntryPlan, EntryWarning};

/// Build the entry root plan for reachability analysis (pipeline step 8).
///
/// # Errors
///
/// Returns [`EntryError`] only when an internal invariant is violated.
pub fn build_entry_roots(
    config: &YokeiConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    plugins: &PluginHints,
    production: bool,
) -> Result<EntryPlan, EntryError> {
    let known_paths = known_file_paths(sources);
    let mut warnings = Vec::new();
    let mut candidates = Vec::new();

    collect_config_entries(config, sources, &known_paths, &mut candidates);
    collect_manifest_entries(
        manifest,
        sources,
        &known_paths,
        &mut candidates,
        &mut warnings,
    );
    collect_plugin_entries(plugins, &mut candidates);
    collect_symbol_ref_entries(
        plugins,
        sources,
        &known_paths,
        &mut candidates,
        &mut warnings,
    );
    candidates.extend(detect_auto_entries(sources));

    if production {
        candidates.retain(|candidate| candidate.context.is_included_in_production());
    }

    let mode = resolve_project_mode(config, manifest, sources, &candidates, &mut warnings);
    let mut roots = merge_entry_candidates(candidates);
    roots.sort_by(|left, right| left.spec.path.cmp(&right.spec.path));
    roots.retain(|root| retain_existing_root(root, &known_paths, &mut warnings));

    Ok(EntryPlan {
        mode,
        roots,
        warnings,
    })
}

fn known_file_paths(sources: &DiscoveredSources) -> BTreeSet<String> {
    sources.files.iter().map(|file| file.path.clone()).collect()
}

fn collect_config_entries(
    config: &YokeiConfig,
    sources: &DiscoveredSources,
    known_paths: &BTreeSet<String>,
    candidates: &mut Vec<EntryCandidate>,
) {
    for entry in &config.entry {
        let context = context_for_path(&entry.path, sources, known_paths);
        candidates.push(EntryCandidate {
            spec: entry.clone(),
            context,
            origin: EntryOrigin::Config,
        });
    }
}

fn collect_manifest_entries(
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    known_paths: &BTreeSet<String>,
    candidates: &mut Vec<EntryCandidate>,
    warnings: &mut Vec<EntryWarning>,
) {
    for entry_point in &manifest.entry_points {
        let Some((module, symbol)) = parse_manifest_target(&entry_point.target) else {
            continue;
        };
        let Some(path) = resolve_module_to_path(&module, &sources.layout, known_paths) else {
            warnings.push(EntryWarning::UnresolvedModuleTarget {
                module: module.clone(),
                origin: format!("{}.{}", entry_point.group, entry_point.name),
            });
            continue;
        };
        candidates.push(EntryCandidate {
            spec: EntrySpec {
                path,
                symbol: symbol.clone(),
            },
            context: FileContext::Runtime,
            origin: EntryOrigin::Manifest {
                name: entry_point.name.clone(),
                group: entry_point.group.clone(),
            },
        });
    }
}

fn collect_plugin_entries(plugins: &PluginHints, candidates: &mut Vec<EntryCandidate>) {
    for contrib in &plugins.contributions {
        for entry in &contrib.entries {
            candidates.push(EntryCandidate {
                spec: entry.spec.clone(),
                context: entry.context,
                origin: EntryOrigin::Plugin {
                    plugin: contrib.plugin,
                    label: entry.origin.label.clone(),
                },
            });
        }
    }
}

fn collect_symbol_ref_entries(
    plugins: &PluginHints,
    sources: &DiscoveredSources,
    known_paths: &BTreeSet<String>,
    candidates: &mut Vec<EntryCandidate>,
    warnings: &mut Vec<EntryWarning>,
) {
    for symbol_ref in plugins.symbol_refs() {
        let Some(path) = resolve_module_to_path(&symbol_ref.module, &sources.layout, known_paths)
        else {
            warnings.push(EntryWarning::UnresolvedModuleTarget {
                module: symbol_ref.module.clone(),
                origin: symbol_ref.origin.label.clone(),
            });
            continue;
        };
        let context = context_for_path(&path, sources, known_paths);
        candidates.push(EntryCandidate {
            spec: EntrySpec {
                path,
                symbol: Some(symbol_ref.symbol.clone()),
            },
            context,
            origin: EntryOrigin::SymbolRef {
                module: symbol_ref.module.clone(),
                symbol: symbol_ref.symbol.clone(),
                label: symbol_ref.origin.label.clone(),
            },
        });
    }
}

fn parse_manifest_target(target: &str) -> Option<(String, Option<String>)> {
    if let Some((module, symbol)) = parse_uvicorn_script_target(target) {
        return Some((module, Some(symbol)));
    }
    if let Some((module, symbol)) = parse_module_symbol(target) {
        return Some((module, Some(symbol)));
    }
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some((trimmed.to_owned(), None))
}

fn context_for_path(
    path: &str,
    sources: &DiscoveredSources,
    known_paths: &BTreeSet<String>,
) -> FileContext {
    if known_paths.contains(path) {
        sources
            .files
            .iter()
            .find(|file| file.path == path)
            .map_or_else(
                || assign_file_context(path, &sources.layout),
                |file| file.context,
            )
    } else {
        assign_file_context(path, &sources.layout)
    }
}

fn retain_existing_root(
    root: &super::types::EntryRoot,
    known_paths: &BTreeSet<String>,
    warnings: &mut Vec<EntryWarning>,
) -> bool {
    if known_paths.contains(&root.spec.path) {
        return true;
    }
    warnings.push(EntryWarning::MissingEntryPath {
        path: root.spec.path.clone(),
    });
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::manifest::{LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata};
    use crate::plugins::PluginHints;
    use crate::sources::{
        DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, ProjectLayout,
    };

    fn minimal_sources(files: Vec<DiscoveredFile>) -> DiscoveredSources {
        DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files,
            warnings: Vec::new(),
        }
    }

    fn empty_manifest() -> LoadedManifest {
        LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn production_excludes_test_entries() {
        let sources = minimal_sources(vec![
            DiscoveredFile {
                path: "src/acme/__init__.py".to_owned(),
                kind: FileKind::Python,
                context: FileContext::Runtime,
            },
            DiscoveredFile {
                path: "tests/conftest.py".to_owned(),
                kind: FileKind::Python,
                context: FileContext::Test,
            },
        ]);
        let plan = build_entry_roots(
            &default_config(),
            &empty_manifest(),
            &sources,
            &PluginHints {
                contributions: Vec::new(),
                warnings: Vec::new(),
            },
            true,
        )
        .expect("plan");
        assert!(
            plan.roots
                .iter()
                .all(|root| root.context.is_included_in_production())
        );
    }
}
