//! Build entry roots from config, manifest, plugins, and auto-detection.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::{ChokkinConfig, EntrySpec, ProjectMode};
use crate::manifest::LoadedManifest;
use crate::plugins::{PluginHints, parse_module_symbol, parse_uvicorn_script_target};
use crate::sources::{DiscoveredSources, FileContext, assign_file_context};

use super::auto::detect_auto_entries;
use super::merge::merge_entry_candidates;
use super::mode::resolve_project_mode;
use super::module::resolve_module_to_path;
use super::types::{EntryCandidate, EntryOrigin, EntryPlan, EntryRoot, EntryWarning};

/// Build the entry root plan for reachability analysis (pipeline step 8).
#[must_use]
pub fn build_entry_roots(
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    plugins: &PluginHints,
    production: bool,
) -> EntryPlan {
    let known_paths = known_file_paths(sources);
    let file_contexts = file_context_index(sources);
    let mut warnings = Vec::new();
    let mut candidates = Vec::new();

    collect_config_entries(config, &file_contexts, &mut candidates);
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
        &file_contexts,
        &mut candidates,
        &mut warnings,
    );
    candidates.extend(detect_auto_entries(sources));

    if production {
        candidates.retain(|candidate| candidate.context.is_included_in_production());
    }

    let mode = resolve_project_mode(config, manifest, sources, &candidates, &mut warnings);
    if mode == ProjectMode::Library {
        collect_library_package_entries(sources, &file_contexts, &mut candidates);
    }
    let mut roots = merge_entry_candidates(candidates);
    roots.retain(|root| retain_existing_root(root, &known_paths, &mut warnings));

    EntryPlan {
        mode,
        roots,
        warnings,
        library_members: Vec::new(),
    }
}

/// Add each workspace member's own manifest entry points as roots of `plan`.
///
/// The root manifest never names a member's `[project.scripts]`, so without
/// this every file of an app member is an orphan (#488).
pub fn add_member_manifest_roots<'a>(
    plan: &mut EntryPlan,
    root_sources: &DiscoveredSources,
    members: impl IntoIterator<Item = (&'a str, &'a LoadedManifest, &'a DiscoveredSources)>,
) {
    let root_paths = known_file_paths(root_sources);
    for (member_path, manifest, sources) in members {
        let mut candidates = Vec::new();
        collect_manifest_entries(
            manifest,
            sources,
            &known_file_paths(sources),
            &mut candidates,
            &mut Vec::new(),
        );
        for candidate in candidates {
            let path = format!("{member_path}/{}", candidate.spec.path);
            if !root_paths.contains(&path) {
                continue;
            }
            if let Some(root) = plan.roots.iter_mut().find(|root| root.spec.path == path) {
                if !root.origins.contains(&candidate.origin) {
                    root.origins.push(candidate.origin);
                }
                continue;
            }
            plan.roots.push(EntryRoot {
                spec: EntrySpec {
                    path,
                    symbol: candidate.spec.symbol,
                },
                context: candidate.context,
                origins: vec![candidate.origin],
            });
        }
    }
    plan.roots
        .sort_by(|left, right| left.spec.path.cmp(&right.spec.path));
}

fn known_file_paths(sources: &DiscoveredSources) -> BTreeSet<String> {
    sources.files.iter().map(|file| file.path.clone()).collect()
}

fn file_context_index(sources: &DiscoveredSources) -> BTreeMap<&str, FileContext> {
    sources
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.context))
        .collect()
}

fn collect_config_entries(
    config: &ChokkinConfig,
    file_contexts: &BTreeMap<&str, FileContext>,
    candidates: &mut Vec<EntryCandidate>,
) {
    for entry in &config.entry {
        let context = context_for_path(&entry.path, file_contexts);
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

/// A library's public API is its own entry point: without this, a library
/// with no script, test, or config root reaches nothing and every runtime
/// dependency it imports reads as unused (#501).
fn collect_library_package_entries(
    sources: &DiscoveredSources,
    file_contexts: &BTreeMap<&str, FileContext>,
    candidates: &mut Vec<EntryCandidate>,
) {
    for package in &sources.layout.packages {
        let prefix = format!("{}/", sources.layout.package_dir(package));
        let inits = file_contexts
            .range(prefix.as_str()..)
            .take_while(|(path, _)| path.starts_with(&prefix))
            .filter(|(path, context)| {
                path.ends_with("/__init__.py") && **context == FileContext::Runtime
            });
        for (path, _) in inits {
            candidates.push(EntryCandidate {
                spec: EntrySpec {
                    path: (*path).to_owned(),
                    symbol: None,
                },
                context: FileContext::Runtime,
                origin: EntryOrigin::Auto {
                    rule: "auto:library package".to_owned(),
                },
            });
        }
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

#[allow(clippy::too_many_arguments)]
fn collect_symbol_ref_entries(
    plugins: &PluginHints,
    sources: &DiscoveredSources,
    known_paths: &BTreeSet<String>,
    file_contexts: &BTreeMap<&str, FileContext>,
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
        let context = context_for_path(&path, file_contexts);
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

fn context_for_path(path: &str, file_contexts: &BTreeMap<&str, FileContext>) -> FileContext {
    file_contexts
        .get(path)
        .copied()
        .unwrap_or_else(|| assign_file_context(path))
}

fn retain_existing_root(
    root: &EntryRoot,
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
