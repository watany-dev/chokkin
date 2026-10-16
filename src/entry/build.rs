//! Build entry roots from config, manifest, plugins, and auto-detection.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::{ChokkinConfig, EntrySpec, ProjectMode};
use crate::manifest::LoadedManifest;
use crate::path_util::{is_submodule, join_rel};
use crate::plugins::{PluginHints, parse_module_symbol, parse_uvicorn_script_target};
use crate::sources::{DiscoveredSources, FileContext, assign_file_context};

use super::auto::detect_auto_entries;
use super::merge::merge_entry_candidates;
use super::mode::{is_library_member, resolve_project_mode};
use super::module::resolve_module_to_path;
use super::types::{EntryCandidate, EntryOrigin, EntryPlan, EntryRoot, EntryWarning};

/// A workspace member as entry construction reads it: its root-relative
/// directory, manifest, and member-relative sources.
pub(crate) type MemberEntryInput<'a> = (&'a str, &'a LoadedManifest, &'a DiscoveredSources);

/// Build the entry root plan for reachability analysis (pipeline step 8).
///
/// `members` are the workspace members: their own entry points become roots
/// and, under `mode = auto`, those that ship as libraries become
/// `library_members`.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_entry_roots(
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    members: &[MemberEntryInput<'_>],
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
    candidates.extend(
        build_script_entries(manifest)
            .filter(|candidate| known_paths.contains(&candidate.spec.path)),
    );
    collect_plugin_entries(plugins, &mut candidates);
    collect_command_entries(plugins, &known_paths, &mut candidates);
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

    let mode = resolve_project_mode(config, manifest, sources, &candidates);
    if mode == ProjectMode::Library {
        collect_library_package_entries(sources, &file_contexts, &mut candidates);
    }
    let mut member_warnings = Vec::new();
    collect_member_entries(members, &known_paths, &mut candidates, &mut member_warnings);
    let mut roots = merge_entry_candidates(candidates);
    roots.retain(|root| retain_existing_root(root, &known_paths, &mut warnings));
    warnings.append(&mut member_warnings);

    EntryPlan {
        mode,
        roots,
        warnings,
        library_members: library_members(config, members),
    }
}

/// Members that ship as their own libraries, whatever mode the project
/// itself resolves to (#488). An explicit `mode` applies to every member.
fn library_members(config: &ChokkinConfig, members: &[MemberEntryInput<'_>]) -> Vec<String> {
    if config.mode != ProjectMode::Auto {
        return Vec::new();
    }
    members
        .iter()
        .filter(|(_, manifest, sources)| is_library_member(manifest, sources))
        .map(|(path, ..)| (*path).to_owned())
        .collect()
}

/// Each workspace member's own manifest entry points, as root-relative
/// candidates.
///
/// The root manifest never names a member's `[project.scripts]`, so without
/// these every file of an app member is an orphan (#488). They join after
/// mode resolution, which reads the root's own entries only.
fn collect_member_entries(
    members: &[MemberEntryInput<'_>],
    root_paths: &BTreeSet<String>,
    candidates: &mut Vec<EntryCandidate>,
    warnings: &mut Vec<EntryWarning>,
) {
    for (member_path, manifest, sources) in members {
        let mut member_candidates = Vec::new();
        let mut member_warnings = Vec::new();
        collect_manifest_entries(
            manifest,
            sources,
            &known_file_paths(sources),
            &mut member_candidates,
            &mut member_warnings,
        );
        for warning in &mut member_warnings {
            if let EntryWarning::UnresolvedModuleTarget { file, .. } = warning {
                *file = join_rel(member_path, file);
            }
        }
        warnings.append(&mut member_warnings);
        member_candidates.extend(build_script_entries(manifest));
        for mut candidate in member_candidates {
            candidate.spec.path = format!("{member_path}/{}", candidate.spec.path);
            if root_paths.contains(&candidate.spec.path) {
                candidates.push(candidate);
            }
        }
    }
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
                file: entry_point.origin.file.clone(),
                label: entry_point.origin.label.clone(),
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

/// hatchling loads its build hooks by path, so nothing imports them (#735).
fn build_script_entries(manifest: &LoadedManifest) -> impl Iterator<Item = EntryCandidate> + '_ {
    manifest
        .metadata
        .build_scripts
        .iter()
        .map(|script| EntryCandidate {
            spec: EntrySpec {
                path: script.path.clone(),
                symbol: None,
            },
            context: FileContext::Dev,
            origin: EntryOrigin::Manifest {
                name: "path".to_owned(),
                group: script.table.clone(),
            },
        })
}

/// Rule of a library root for a subpackage `__init__.py` below the package's
/// own: callers may import it, but nothing shows the package loads it (#802).
pub(crate) const LIBRARY_SUBPACKAGE_RULE: &str = "library subpackage";

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
        let in_package = |init: &str| {
            init.starts_with(&prefix) && file_contexts.get(init) == Some(&FileContext::Runtime)
        };
        for (path, _) in inits {
            let rule = if is_submodule(path, in_package) {
                LIBRARY_SUBPACKAGE_RULE
            } else {
                "library package"
            };
            candidates.push(EntryCandidate {
                spec: EntrySpec {
                    path: (*path).to_owned(),
                    symbol: None,
                },
                context: FileContext::Runtime,
                origin: EntryOrigin::Auto {
                    rule: rule.to_owned(),
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

/// Commands run their scripts by hand or in CI, so like hatchling's build
/// scripts (#735) those files are dev (#797). A module of a package keeps its
/// context, since the package may import it too.
pub(crate) fn mark_command_scripts_dev(sources: &mut DiscoveredSources, plugins: &PluginHints) {
    let known_paths = known_file_paths(sources);
    let scripts: BTreeSet<String> = plugins
        .config_script_entries
        .iter()
        .filter_map(|script| command_script_path(&script.paths, &known_paths))
        .filter(|path| {
            let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
            !known_paths.contains(&join_rel(dir, "__init__.py"))
        })
        .cloned()
        .collect();
    for file in &mut sources.files {
        if file.context == FileContext::Runtime && scripts.contains(&file.path) {
            file.context = FileContext::Dev;
        }
    }
}

fn command_script_path<'a>(
    paths: &'a [String],
    known_paths: &BTreeSet<String>,
) -> Option<&'a String> {
    paths.iter().find(|path| known_paths.contains(*path))
}

fn collect_command_entries(
    plugins: &PluginHints,
    known_paths: &BTreeSet<String>,
    candidates: &mut Vec<EntryCandidate>,
) {
    for script in &plugins.config_script_entries {
        let Some(path) = command_script_path(&script.paths, known_paths) else {
            continue;
        };
        candidates.push(EntryCandidate {
            spec: EntrySpec {
                path: path.clone(),
                symbol: None,
            },
            context: FileContext::Dev,
            origin: EntryOrigin::Command {
                label: match script.origin.line {
                    Some(line) => {
                        format!("{} ({}:{line})", script.origin.label, script.origin.file)
                    },
                    None => format!("{} ({})", script.origin.label, script.origin.file),
                },
            },
        });
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
                file: symbol_ref.origin.file.clone(),
                label: symbol_ref.origin.label.clone(),
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
    if known_paths.contains(&root.path) {
        return true;
    }
    warnings.push(EntryWarning::MissingEntryPath {
        path: root.path.clone(),
    });
    false
}
