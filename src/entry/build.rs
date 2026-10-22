//! Build entry roots from config, manifest, plugins, and auto-detection.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::{ChokkinConfig, EntrySpec, ProjectMode};
use crate::manifest::{DeclaredDependency, LoadedManifest, normalize_distribution_name};
use crate::parser::ParseSummary;
use crate::path_util::{is_submodule, join_rel};
use crate::plugins::{
    CommandTarget, CommandTargetRef, PluginHints, ReferenceOrigin, parse_module_symbol,
    parse_uvicorn_script_target,
};
use crate::sources::{
    DiscoveredSources, FileContext, LayoutInfo, PublicSurface, assign_file_context, is_dockerfile,
};

use super::auto::{detect_auto_entries, is_app_entry_file};
use super::merge::merge_entry_candidates;
use super::mode::{
    is_distributed_library, is_library_member, metapackage_distributions, resolve_project_mode,
};
use super::module::{resolve_module_on_disk, resolve_module_to_path};
use super::types::{
    DistributedMember, EntryCandidate, EntryOrigin, EntryPlan, EntryRoot, EntryWarning,
};

/// A workspace member as entry construction reads it: its root-relative
/// directory, manifest, and member-relative sources.
pub(crate) type MemberEntryInput<'a> = (&'a str, &'a LoadedManifest, &'a DiscoveredSources);

/// Build the entry root plan for reachability analysis (pipeline step 8).
///
/// `members` are the workspace members: their own entry points become roots
/// and, under `mode = auto`, those that ship as libraries become
/// `library_members`.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "takes one output per earlier pipeline phase; a bundle struct would only rename them"
)]
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
        production,
        &mut candidates,
        &mut warnings,
    );
    candidates.extend(
        build_script_entries(manifest)
            .filter(|candidate| known_paths.contains(&candidate.spec.path)),
    );
    collect_plugin_entries(plugins, &mut candidates);
    collect_command_entries(
        plugins,
        &sources.layout,
        &known_paths,
        &file_contexts,
        &mut candidates,
    );
    collect_server_app_entries(
        plugins,
        sources,
        &known_paths,
        &file_contexts,
        production,
        &mut candidates,
        &mut warnings,
    );
    collect_symbol_ref_entries(
        plugins,
        sources,
        &known_paths,
        &file_contexts,
        production,
        &mut candidates,
        &mut warnings,
    );
    candidates.extend(detect_auto_entries(sources));

    if production {
        candidates.retain(|candidate| candidate.context.is_included_in_production());
    }

    let mode = resolve_project_mode(config, manifest, sources, &candidates);
    let distributed = if mode == ProjectMode::App {
        metapackage_distributions(config, manifest, sources, &candidates)
    } else {
        BTreeMap::new()
    };
    if mode == ProjectMode::Library {
        collect_library_package_entries(sources, &file_contexts, &mut candidates);
    }
    let mut member_warnings = Vec::new();
    collect_member_entries(
        members,
        &known_paths,
        production,
        &mut candidates,
        &mut member_warnings,
    );
    let mut roots = merge_entry_candidates(candidates);
    roots.retain(|root| retain_existing_root(root, &known_paths, &mut warnings));
    warnings.append(&mut member_warnings);

    EntryPlan {
        mode,
        roots,
        warnings,
        library_members: library_members(config, members),
        distributed_members: distributed_members(members, &distributed),
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

/// Members that a metapackage root distributes (#1060), each with the
/// line naming the requirement. `distributed` is empty unless `mode` is
/// `auto`, so an explicit `mode` applies to every member here too.
fn distributed_members(
    members: &[MemberEntryInput<'_>],
    distributed: &BTreeMap<String, &DeclaredDependency>,
) -> Vec<DistributedMember> {
    members
        .iter()
        .filter_map(|(path, manifest, sources)| {
            let name = normalize_distribution_name(manifest.metadata.name.as_deref()?);
            let dep = distributed.get(&name)?;
            is_distributed_library(manifest, sources).then(|| DistributedMember {
                member: (*path).to_owned(),
                reason: distributed_member_reason(path, dep),
            })
        })
        .collect()
}

/// The `--trace` / `--explain` line for an orphan that the wheel of a member
/// a metapackage root distributes ships (#1060).
fn distributed_member_reason(member: &str, dep: &DeclaredDependency) -> String {
    let at = dep
        .origin
        .line
        .map_or_else(String::new, |line| format!(":{line}"));
    format!(
        "confidence: library (workspace member `{member}` is `{name}`, which the metapackage root distributes as `{label}` in `{file}{at}`, and its wheel ships this file)",
        name = dep.name,
        label = dep.origin.label,
        file = dep.origin.file,
    )
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
    production: bool,
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
            production,
            &mut member_candidates,
            &mut member_warnings,
        );
        for warning in &mut member_warnings {
            match warning {
                EntryWarning::UnresolvedModuleTarget { file, .. } => {
                    *file = join_rel(member_path, file);
                },
                EntryWarning::MissingEntryPath { path } => *path = join_rel(member_path, path),
            }
        }
        warnings.append(&mut member_warnings);
        for mut candidate in member_candidates {
            candidate.spec.path = format!("{member_path}/{}", candidate.spec.path);
            if root_paths.contains(&candidate.spec.path) {
                candidates.push(candidate);
            } else {
                // The member's own walk found the file but the root's did not
                // (a root `exclude`): it is skipped like a root entry is (#934).
                warnings.push(EntryWarning::MissingEntryPath {
                    path: candidate.spec.path,
                });
            }
        }
        // A build script the root's walk does not find is dropped, as the
        // root's own is; `../` paths name no file of the walk at all. Under
        // `--production` it is a dev file, held back as the root's is.
        candidates.extend(
            build_script_entries(manifest)
                .filter(|candidate| !production || candidate.context.is_included_in_production())
                .map(|mut candidate| {
                    candidate.spec.path = format!("{member_path}/{}", candidate.spec.path);
                    candidate
                })
                .filter(|candidate| root_paths.contains(&candidate.spec.path)),
        );
    }
}

/// A target that maps to no discovered file: skipped when a file is there
/// that discovery left out, its imports unread like a skipped path entry's;
/// unmapped when no file is, since it runs nothing chokkin could read (#933).
/// The file may also sit beside the `.py` file or Dockerfile that names it.
/// `--production` leaves out a test or dev file on purpose, so its target is
/// unmapped as it was before #933, not a skipped read.
fn unresolved_target(
    module: &str,
    file: &str,
    label: &str,
    sources: &DiscoveredSources,
    production: bool,
) -> EntryWarning {
    let on_disk = |path: &str| {
        path.split('/')
            .fold(sources.root.path.clone(), |dir, part| dir.join(part))
            .is_file()
    };
    let path = resolve_module_on_disk(module, &sources.layout, &sources.root.path)
        .or_else(|| beside_origin_where(module, file, &on_disk));
    match path {
        Some(path) if !production || assign_file_context(&path).is_included_in_production() => {
            EntryWarning::MissingEntryPath { path }
        },
        _ => EntryWarning::UnresolvedModuleTarget {
            module: module.to_owned(),
            file: file.to_owned(),
            label: label.to_owned(),
        },
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

#[expect(
    clippy::too_many_arguments,
    reason = "split out of its caller, it takes the caller's locals and output buffers as they are"
)]
fn collect_manifest_entries(
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    known_paths: &BTreeSet<String>,
    production: bool,
    candidates: &mut Vec<EntryCandidate>,
    warnings: &mut Vec<EntryWarning>,
) {
    for entry_point in &manifest.entry_points {
        let Some((module, symbol)) = parse_manifest_target(&entry_point.target) else {
            continue;
        };
        let Some(path) = resolve_module_to_path(&module, &sources.layout, known_paths) else {
            warnings.push(unresolved_target(
                &module,
                &entry_point.origin.file,
                &entry_point.origin.label,
                sources,
                production,
            ));
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
/// scripts (#735) those files are dev (#797), and so are the scripts and `-m`
/// modules task files run (#912). A module of a package keeps its context,
/// since the package may import it too, and so does a file another root runs
/// in production: the root `main.py` a workflow also runs is still the app,
/// and so is a script a Dockerfile or Procfile runs. A task file's target
/// with a `__main__` block or one the wheel ships is also kept, since a
/// Makefile often runs the app itself; a CI script keeps being dev (#797).
pub(crate) fn mark_command_scripts_dev(
    sources: &mut DiscoveredSources,
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    plugins: &PluginHints,
    parse: &ParseSummary,
) {
    let known_paths = known_file_paths(sources);
    let runtime_roots = runtime_root_paths(sources, config, manifest, plugins, &known_paths);
    let shipped = PublicSurface::resolve(manifest.metadata.wheel_targets.as_ref(), &sources.files);
    let app_files: BTreeSet<String> = parse
        .modules
        .iter()
        .filter(|module| module.has_main_block)
        .map(|module| module.path.clone())
        .chain(shipped.into_iter().flat_map(|surface| surface.files))
        .collect();
    let task_targets = plugins
        .config_command_targets
        .iter()
        .filter_map(|command| command_target_path(command, &sources.layout, &known_paths))
        .filter(|path| !app_files.contains(path));
    let scripts: BTreeSet<String> = plugins
        .config_script_entries
        .iter()
        .filter_map(|script| command_script_path(&script.paths, &known_paths).cloned())
        .chain(task_targets)
        .filter(|path| {
            let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
            !known_paths.contains(&join_rel(dir, "__init__.py")) && !runtime_roots.contains(path)
        })
        .collect();
    for file in &mut sources.files {
        if file.context == FileContext::Runtime && scripts.contains(&file.path) {
            file.context = FileContext::Dev;
        }
    }
}

/// Files that roots other than dev commands run whatever context the files
/// have: configured entries, manifest entry points, runtime plugin entries,
/// symbol references, server apps, what deploy commands run, and app files
/// found by name (`main.py`).
fn runtime_root_paths(
    sources: &DiscoveredSources,
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    plugins: &PluginHints,
    known_paths: &BTreeSet<String>,
) -> BTreeSet<String> {
    let resolve = |module: &str| resolve_module_to_path(module, &sources.layout, known_paths);
    let manifest_paths = manifest.entry_points.iter().filter_map(|entry_point| {
        parse_manifest_target(&entry_point.target).and_then(|(module, _)| resolve(&module))
    });
    let plugin_paths = plugins
        .contributions
        .iter()
        .flat_map(|contrib| &contrib.entries)
        .filter(|entry| entry.context == FileContext::Runtime)
        .map(|entry| entry.spec.path.clone());
    let symbol_paths = plugins
        .symbol_refs()
        .filter_map(|symbol_ref| resolve(&symbol_ref.module));
    let command_paths = plugins
        .config_command_targets
        .iter()
        .filter_map(|command| match &command.target {
            CommandTarget::App { module, .. } => resolve(module),
            _ if command.deploy => command_target_path(command, &sources.layout, known_paths),
            _ => None,
        });
    config
        .entry
        .iter()
        .map(|entry| entry.path.clone())
        .chain(manifest_paths)
        .chain(plugin_paths)
        .chain(symbol_paths)
        .chain(command_paths)
        .chain(
            sources
                .python_files()
                .filter(|file| is_app_entry_file(&file.path, &sources.layout))
                .map(|file| file.path.clone()),
        )
        .collect()
}

fn command_script_path<'a>(
    paths: &'a [String],
    known_paths: &BTreeSet<String>,
) -> Option<&'a String> {
    paths.iter().find(|path| known_paths.contains(*path))
}

/// The file a task file or tool config runs as a script or with `-m`
/// (#912): a script's first discovered candidate, or the module's
/// `__main__` before the module itself, as `python -m` runs a package's.
fn command_target_path(
    command: &CommandTargetRef,
    layout: &LayoutInfo,
    known_paths: &BTreeSet<String>,
) -> Option<String> {
    match &command.target {
        CommandTarget::Script { paths } => command_script_path(paths, known_paths).cloned(),
        CommandTarget::Run { module } => {
            resolve_module_to_path(&format!("{module}.__main__"), layout, known_paths)
                .or_else(|| resolve_module_to_path(module, layout, known_paths))
        },
        CommandTarget::Module { .. } | CommandTarget::App { .. } | CommandTarget::Opaque { .. } => {
            None
        },
    }
}

/// Scripts CI workflows run (#797) and the scripts and `-m` modules task
/// files and tool configs run (#912) are roots, so what they import is
/// followed. They are dev unless a deploy command runs them: those keep the
/// file's own context, as a server's app does (#1004). A target that names no
/// discovered file adds nothing; the gap stage reads the ones that resolve
/// but are relocated, imported by `-c` code, or loaded by a task runner.
fn collect_command_entries(
    plugins: &PluginHints,
    layout: &LayoutInfo,
    known_paths: &BTreeSet<String>,
    file_contexts: &BTreeMap<&str, FileContext>,
    candidates: &mut Vec<EntryCandidate>,
) {
    let scripts = plugins.config_script_entries.iter().filter_map(|script| {
        let path = command_script_path(&script.paths, known_paths)?;
        Some((path.clone(), FileContext::Dev, &script.origin))
    });
    let targets = plugins.config_command_targets.iter().filter_map(|command| {
        let path = command_target_path(command, layout, known_paths)?;
        let context = if command.deploy {
            context_for_path(&path, file_contexts)
        } else {
            FileContext::Dev
        };
        Some((path, context, &command.origin))
    });
    for (path, context, origin) in scripts.chain(targets) {
        candidates.push(EntryCandidate {
            spec: EntrySpec { path, symbol: None },
            context,
            origin: EntryOrigin::Command {
                label: command_label(origin),
            },
        });
    }
}

/// `label (file:line)`, as `--trace` shows a command origin.
fn command_label(origin: &ReferenceOrigin) -> String {
    match origin.line {
        Some(line) => format!("{} ({}:{line})", origin.label, origin.file),
        None => format!("{} ({})", origin.label, origin.file),
    }
}

/// The apps WSGI/ASGI servers load, from commands and literal
/// `uvicorn.run`/`uvicorn.Config` calls (#1004). The server imports the module
/// and reads the attribute, so the file is a root with that symbol, in the
/// file's own context: `CMD ["uvicorn", "pkg.asgi:app"]` serves production
/// code. A `uvicorn.run` call in a script also finds the module next to the
/// script, which `python script.py` puts on `sys.path`, and a Dockerfile next
/// to itself, its usual build context (#1061). A module that maps to
/// no discovered file is a third-party or missing app and adds nothing,
/// unless the file is on disk and discovery left it out.
#[expect(
    clippy::too_many_arguments,
    reason = "split out of its caller, it takes the caller's locals and output buffers as they are"
)]
fn collect_server_app_entries(
    plugins: &PluginHints,
    sources: &DiscoveredSources,
    known_paths: &BTreeSet<String>,
    file_contexts: &BTreeMap<&str, FileContext>,
    production: bool,
    candidates: &mut Vec<EntryCandidate>,
    warnings: &mut Vec<EntryWarning>,
) {
    for command in &plugins.config_command_targets {
        let CommandTarget::App { module, symbol } = &command.target else {
            continue;
        };
        let Some(path) = resolve_module_to_path(module, &sources.layout, known_paths)
            .or_else(|| beside_origin(module, &command.origin.file, known_paths))
        else {
            let warning = unresolved_target(
                module,
                &command.origin.file,
                &command.origin.label,
                sources,
                production,
            );
            if matches!(warning, EntryWarning::MissingEntryPath { .. }) {
                warnings.push(warning);
            }
            continue;
        };
        let context = context_for_path(&path, file_contexts);
        candidates.push(EntryCandidate {
            spec: EntrySpec {
                path,
                symbol: symbol.clone(),
            },
            context,
            origin: EntryOrigin::Command {
                label: command_label(&command.origin),
            },
        });
    }
}

/// The discovered module `module` names in the directory of `origin`, a `.py`
/// file or a Dockerfile.
pub(crate) fn beside_origin(
    module: &str,
    origin: &str,
    known_paths: &BTreeSet<String>,
) -> Option<String> {
    beside_origin_where(module, origin, &|path| known_paths.contains(path))
}

fn beside_origin_where(
    module: &str,
    origin: &str,
    exists: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let origin = std::path::Path::new(origin);
    if origin.extension().is_none_or(|ext| ext != "py") && !is_dockerfile(origin) {
        return None;
    }
    let dir = origin.parent()?.to_str()?;
    let stem = join_rel(dir, &module.replace('.', "/"));
    [format!("{stem}.py"), format!("{stem}/__init__.py")]
        .into_iter()
        .find(|path| exists(path))
}

#[expect(
    clippy::too_many_arguments,
    reason = "split out of its caller, it takes the caller's locals and output buffers as they are"
)]
fn collect_symbol_ref_entries(
    plugins: &PluginHints,
    sources: &DiscoveredSources,
    known_paths: &BTreeSet<String>,
    file_contexts: &BTreeMap<&str, FileContext>,
    production: bool,
    candidates: &mut Vec<EntryCandidate>,
    warnings: &mut Vec<EntryWarning>,
) {
    for symbol_ref in plugins.symbol_refs() {
        let Some(path) = resolve_module_to_path(&symbol_ref.module, &sources.layout, known_paths)
        else {
            warnings.push(unresolved_target(
                &symbol_ref.module,
                &symbol_ref.origin.file,
                &symbol_ref.origin.label,
                sources,
                production,
            ));
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
