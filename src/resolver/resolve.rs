//! Import resolution orchestration.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::config::{ChokkinConfig, ResolvedWorkspaceMember, TargetVersion};
use crate::graph::ModuleOrigin;
use crate::manifest::{LoadedManifest, normalize_distribution_name};
use crate::parser::{ImportContext, ParseSummary, import_context_for_file};
use crate::plugins::ModuleReference;
use crate::sources::{DiscoveredFile, DiscoveredSources, path_to_module};

use super::first_party::{is_first_party_import, is_workspace_import, path_source_imports};
use super::maps::{ImportMap, build_binary_map};
use super::pytest_path::PytestImportPaths;
use super::stdlib::StdlibRange;
use super::types::{
    ResolutionIndex, ResolveConfidence, ResolveWarning, ResolvedImport, import_root,
};
use super::venv::load_venv_index;

/// Distributions declared outside the root manifest, for the files they cover.
#[derive(Debug, Default)]
pub struct ScopedDeclarations {
    /// PEP 723 script path → normalized names its block declares; a build
    /// script → its `[build-system].requires` (#735).
    pub scripts: BTreeMap<String, BTreeSet<String>>,
    /// Workspace member id → normalized names its manifest declares.
    pub members: BTreeMap<String, BTreeSet<String>>,
    /// Workspace member id → normalized names its lockfile pins.
    pub member_locks: BTreeMap<String, BTreeSet<String>>,
}

/// Resolve parsed imports and plugin module references to origins and distributions.
///
/// Uses per-file stdlib ranges for PEP 723 scripts and resolves an unmapped
/// root through the script block or member manifest owning the file.
/// A script's `requires-python` decides which modules are stdlib for the
/// imports in that file; every other file uses the project range.
/// `workspace_members` marks cross-member imports as first-party so workspace
/// packages do not become false missing-dependency findings.
#[must_use]
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
    let pytest_paths =
        PytestImportPaths::build(sources).with_sys_path_hints(&sources.root.path, parse);
    let local_modules = local_modules(&sources.files, workspace_members);
    let indexed_roots = indexed_roots(sources);
    let owners = MemberOwners::new(workspace_members);

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
                &owners,
                &import_map,
                &venv_index.imports,
                scoped,
                &pytest_paths,
                &local_modules,
                &indexed_roots,
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
                dynamic.optional,
                dynamic.platform_guarded,
                file_stdlib,
                sources,
                manifest,
                config,
                workspace_members,
                &owners,
                &import_map,
                &venv_index.imports,
                scoped,
                &pytest_paths,
                &local_modules,
                &indexed_roots,
                &mut warnings,
                &mut root_cache,
            ));
        }
        if module.pytest_plugins.is_empty() {
            continue;
        }
        // A plugin name nothing resolves is a distribution missing from the
        // environment, not a broken import in this file.
        let plugin_context = sources
            .files
            .iter()
            .find(|file| file.path == module.path)
            .map_or(ImportContext::Test, |file| {
                import_context_for_file(file.context)
            });
        for plugin in &module.pytest_plugins {
            let mut site_warnings = Vec::new();
            imports.push(resolve_import_site(
                &plugin.module,
                &plugin.module,
                &module.path,
                plugin.line,
                plugin_context,
                false,
                false,
                file_stdlib,
                sources,
                manifest,
                config,
                workspace_members,
                &owners,
                &import_map,
                &venv_index.imports,
                scoped,
                &pytest_paths,
                &local_modules,
                &indexed_roots,
                &mut site_warnings,
                &mut root_cache,
            ));
            warnings.extend(
                site_warnings
                    .into_iter()
                    .filter(|warning| !matches!(warning, ResolveWarning::UnresolvedImport { .. })),
            );
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
            &owners,
            &import_map,
            &venv_index.imports,
            scoped,
            &pytest_paths,
            &local_modules,
            &indexed_roots,
            &mut warnings,
            &mut root_cache,
        ));
    }

    ResolutionIndex {
        imports,
        warnings,
        transitive: manifest.lockfile.clone(),
        binary_resolutions,
        pytest_plugin_distributions: venv_index.pytest_plugins,
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
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
    owners: &MemberOwners<'_>,
    import_map: &ImportMap,
    venv_imports: &BTreeMap<String, Vec<String>>,
    scoped: &ScopedDeclarations,
    pytest_paths: &PytestImportPaths,
    local_modules: &BTreeSet<String>,
    indexed_roots: &BTreeSet<String>,
    warnings: &mut Vec<ResolveWarning>,
    root_cache: &mut RootCache,
) -> ResolvedImport {
    let root_name = import_root(full_module).to_owned();
    let workspace_member = owners.owner(file);
    let member = workspace_member.as_deref();
    let pick = |candidates: &[String]| site_candidate(candidates, file, member, scoped, manifest);
    let core = if let Some(origin) = site_origin(&root_name, file, context, stdlib, pytest_paths) {
        RootResolution {
            origin,
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
            // The cache holds the root's first candidate; the pick is per site.
            (ModuleOrigin::ThirdParty, None) => RootResolution {
                distribution: root_candidates(&root_name, venv_imports, import_map)
                    .map_or(core.distribution, |(candidates, _)| pick(&candidates)),
                ..core
            },
            (ModuleOrigin::Stdlib, _) | (_, None) => core,
            // A first-party root may share its namespace with a distribution
            // (`poetry` and `poetry.core`); the local tree wins when it has the
            // module itself.
            (ModuleOrigin::FirstParty, Some((module, ..))) if local_modules.contains(module) => {
                core
            },
            (_, Some((_, distributions, confidence))) => RootResolution {
                distribution: pick(&distributions),
                ..root_resolution_from_candidates(
                    &root_name,
                    &distributions,
                    Some(confidence),
                    warnings,
                )
            },
        }
    };

    // An exact name in the file's own script block or member manifest beats an
    // affixed root declaration (`pyfoo` must not take a script's `foo`).
    let core = if core.origin == ModuleOrigin::Unknown {
        scoped_declaration(&root_name, file, member, scoped, ScopedMatch::Exact)
            .or_else(|| root_loose_match(&root_name, manifest))
            .or_else(|| scoped_declaration(&root_name, file, member, scoped, ScopedMatch::Loose))
            // A root that reachability maps to a file is local, even beside a
            // member's declared package (`devel-common/src/docs/`, #612), and so
            // is a namespace or script-sibling fallback (#589), or a module in a
            // directory the file's `sys.path` edits add (#719).
            .or_else(|| {
                (indexed_roots.contains(&root_name)
                    || pytest_paths.provides_fallback(file, &root_name))
                .then_some(RootResolution {
                    origin: ModuleOrigin::FirstParty,
                    distribution: None,
                    confidence: ResolveConfidence::Certain,
                })
            })
            .or_else(|| contextless_requirement_match(&root_name, manifest))
            .unwrap_or(core)
    } else {
        core
    };

    if core.origin == ModuleOrigin::Unknown {
        warnings.push(ResolveWarning::UnresolvedImport {
            import: root_name.clone(),
            file: file.to_owned(),
            line,
            context,
            optional,
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

/// Origin fixed by the import site itself, before any root lookup.
fn site_origin(
    root_name: &str,
    file: &str,
    context: ImportContext,
    stdlib: StdlibRange,
    pytest_paths: &PytestImportPaths,
) -> Option<ModuleOrigin> {
    // `_typeshed` exists only in typeshed's stubs: a checker resolves it, the
    // interpreter never does, so only a runtime import of it is broken (#584).
    if context == ImportContext::Type && root_name == "_typeshed" {
        return Some(ModuleOrigin::Stdlib);
    }
    // pytest puts the test's basedir on `sys.path` ahead of site-packages, so
    // a local module there shadows any distribution of the same name.
    (!stdlib.contains(root_name) && pytest_paths.provides_root(file, root_name))
        .then_some(ModuleOrigin::FirstParty)
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

    if let Some((distributions, confidence)) = root_candidates(root_name, venv_imports, import_map)
    {
        return root_resolution_from_candidates(root_name, &distributions, confidence, warnings);
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

    // The affixed-name step runs per site in [`resolve_import_site`], after a
    // script or member's exact declaration has had its chance.
    RootResolution {
        origin: ModuleOrigin::Unknown,
        distribution: None,
        confidence: ResolveConfidence::Maybe,
    }
}

/// Affixed counterpart of the declared-or-locked step in
/// [`resolve_import_root`] (`pyfoo` for `import foo`).
fn root_loose_match(root_name: &str, manifest: &LoadedManifest) -> Option<RootResolution> {
    let declared = manifest
        .dependencies
        .iter()
        .map(|dep| normalize_distribution_name(&dep.name));
    let locked = manifest.lockfile.edges.keys().cloned();
    loose_declared_match(
        &normalize_distribution_name(root_name),
        declared.chain(locked),
    )
}

/// A name declared only in a requirements file whose context is unknown
/// (`requirements/tests.in`, #679) or in `[build-system].requires` (#720)
/// makes the root third-party, exactly or by affix. No distribution is
/// attached, so CHK003–CHK005 never judge the import against a context
/// neither source gives.
fn contextless_requirement_match(
    root_name: &str,
    manifest: &LoadedManifest,
) -> Option<RootResolution> {
    let declared = || {
        let build = manifest.metadata.build_requires.iter();
        let build = build.map(|dep| normalize_distribution_name(&dep.name));
        manifest
            .sources
            .extra_requirements
            .iter()
            .cloned()
            .chain(build)
    };
    let distribution = normalize_distribution_name(root_name);
    let confidence = if declared().any(|name| name == distribution) {
        ResolveConfidence::Likely
    } else {
        loose_declared_match(&distribution, declared())?.confidence
    };
    Some(RootResolution {
        origin: ModuleOrigin::ThirdParty,
        distribution: None,
        confidence,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedMatch {
    Exact,
    Loose,
}

/// Per-file counterpart of the declared-name step in [`resolve_import_root`]:
/// a script block or member manifest declares for its own files only, so it
/// cannot go through the per-root cache.
fn scoped_declaration(
    root_name: &str,
    file: &str,
    workspace_member: Option<&str>,
    scoped: &ScopedDeclarations,
    kind: ScopedMatch,
) -> Option<RootResolution> {
    let distribution = normalize_distribution_name(root_name);
    let declared = || {
        scoped
            .scripts
            .get(file)
            .into_iter()
            .chain(workspace_member.and_then(|member| scoped.members.get(member)))
            .chain(workspace_member.and_then(|member| scoped.member_locks.get(member)))
            .flatten()
    };
    match kind {
        ScopedMatch::Exact => {
            declared()
                .any(|name| *name == distribution)
                .then_some(RootResolution {
                    origin: ModuleOrigin::ThirdParty,
                    distribution: Some(distribution),
                    confidence: ResolveConfidence::Likely,
                })
        },
        ScopedMatch::Loose => loose_declared_match(&distribution, declared().cloned()),
    }
}

/// A declared distribution whose name differs from the import root only by a
/// `py` / `python` affix (`markdown-it-py` for `markdown_it`, `odfpy` for
/// `odf`, `pydocket` for `docket`). Weaker than an exact match, so `Maybe`;
/// limited to declared or locked names so a local module never passes as one
/// (#361).
fn loose_declared_match(
    distribution: &str,
    declared: impl IntoIterator<Item = String>,
) -> Option<RootResolution> {
    declared
        .into_iter()
        .find(|name| affix_stripped(name).any(|stripped| stripped == distribution))
        .map(|name| RootResolution {
            origin: ModuleOrigin::ThirdParty,
            distribution: Some(name),
            confidence: ResolveConfidence::Maybe,
        })
}

/// `name` without a leading `python-` / `py-` / `py`, or without a trailing
/// `-python` / `-py` / `py`. Never yields an empty name.
fn affix_stripped(name: &str) -> impl Iterator<Item = &str> {
    let prefixless = ["python-", "py-", "py"]
        .into_iter()
        .find_map(|prefix| name.strip_prefix(prefix));
    let suffixless = ["-python", "-py", "py"]
        .into_iter()
        .find_map(|suffix| name.strip_suffix(suffix));
    [prefixless, suffixless]
        .into_iter()
        .flatten()
        .filter(|stripped| {
            !stripped.is_empty() && !stripped.starts_with('-') && !stripped.ends_with('-')
        })
}

/// Every module and package importable from the project root or a workspace
/// member, in flat or `src/` layout. Deeper matches such as
/// `tests/fixtures/vendor/poetry/core/` are not importable as `poetry.core`.
fn local_modules(
    files: &[DiscoveredFile],
    workspace_members: &[ResolvedWorkspaceMember],
) -> BTreeSet<String> {
    let members: HashSet<&str> = workspace_members
        .iter()
        .map(|member| member.path.as_str())
        .collect();
    let mut modules = BTreeSet::new();
    for file in files {
        let file = file.path.replace('\\', "/");
        // Member roots by hash lookup per ancestor, not a prefix test against
        // every member (#592).
        let mut starts = vec![0];
        if file.starts_with("src/") {
            starts.push("src/".len());
        }
        for (slash, _) in file.match_indices('/') {
            let dir = &file[..slash];
            if members.contains(dir)
                || dir
                    .strip_suffix("/src")
                    .is_some_and(|member| members.contains(member))
            {
                starts.push(slash + 1);
            }
        }
        for rest in starts.into_iter().map(|start| &file[start..]) {
            let mut parts: Vec<&str> = rest.split('/').collect();
            let leaf = parts.pop().and_then(|leaf| {
                leaf.strip_suffix(".py")
                    .or_else(|| leaf.strip_suffix(".pyi"))
            });
            let mut module = String::new();
            for part in parts.into_iter().chain(leaf) {
                if !module.is_empty() {
                    module.push('.');
                }
                module.push_str(part);
                modules.insert(module.clone());
            }
        }
    }
    modules
}

/// Import roots of the modules [`path_to_module`] names, the same names
/// reachability resolves to files.
fn indexed_roots(sources: &DiscoveredSources) -> BTreeSet<String> {
    sources
        .files
        .iter()
        .filter_map(|file| path_to_module(&file.path, &sources.layout))
        .map(|module| import_root(&module).to_owned())
        .collect()
}

/// Member ids by directory: a file's owner is a hash lookup per ancestor
/// rather than a scan of every member for every import site (#592).
struct MemberOwners<'a>(HashMap<&'a str, &'a str>);

impl<'a> MemberOwners<'a> {
    fn new(members: &'a [ResolvedWorkspaceMember]) -> Self {
        Self(
            members
                .iter()
                .map(|member| (member.path.as_str(), member.id.as_str()))
                .collect(),
        )
    }

    /// The member whose directory is the longest whole-directory prefix of `file`.
    fn owner(&self, file: &str) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        let normalized = file.replace('\\', "/");
        let mut dir = normalized.as_str();
        loop {
            if let Some(id) = self.0.get(dir) {
                return Some((*id).to_owned());
            }
            dir = &dir[..dir.rfind('/')?];
        }
    }
}

/// Distributions the environment, then the maps, say provide `root_name`;
/// the confidence is the map's, `None` for the environment.
fn root_candidates(
    root_name: &str,
    venv_imports: &BTreeMap<String, Vec<String>>,
    import_map: &ImportMap,
) -> Option<(Vec<String>, Option<ResolveConfidence>)> {
    venv_imports
        .get(root_name)
        .map(|distributions| (distributions.clone(), None))
        .or_else(|| {
            import_map
                .candidates(root_name)
                .map(|(distributions, confidence)| (distributions, Some(confidence)))
        })
}

/// Among several distributions providing a root, a declared one beats a
/// locked one, and the file's own script block or member manifest beats the
/// root's (#732); else the first.
fn site_candidate(
    candidates: &[String],
    file: &str,
    member: Option<&str>,
    scoped: &ScopedDeclarations,
    manifest: &LoadedManifest,
) -> Option<String> {
    let in_member = |sets: &BTreeMap<String, BTreeSet<String>>, name: &str| {
        member
            .and_then(|member| sets.get(member))
            .is_some_and(|names| names.contains(name))
    };
    let prefers: [&dyn Fn(&str) -> bool; 5] = [
        &|name| {
            scoped
                .scripts
                .get(file)
                .is_some_and(|names| names.contains(name))
        },
        &|name| in_member(&scoped.members, name),
        &|name| {
            manifest
                .dependencies
                .iter()
                .any(|dep| normalize_distribution_name(&dep.name) == name)
        },
        &|name| in_member(&scoped.member_locks, name),
        &|name| manifest.lockfile.edges.contains_key(name),
    ];
    prefers
        .iter()
        .find_map(|prefers| candidates.iter().find(|name| prefers(name)))
        .or_else(|| candidates.first())
        .cloned()
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

    fn member(id: &str, path: &str) -> ResolvedWorkspaceMember {
        ResolvedWorkspaceMember {
            id: id.to_owned(),
            path: path.to_owned(),
            pyproject_toml: None,
        }
    }

    #[test]
    fn workspace_member_is_the_longest_whole_directory_prefix() {
        let members = [
            member("api", "packages/api"),
            member("plugins", "packages/api/plugins"),
        ];
        let owners = MemberOwners::new(&members);
        let owner = |file: &str| owners.owner(file);
        assert_eq!(owner("packages/api/app.py").as_deref(), Some("api"));
        assert_eq!(
            owner("packages\\api\\plugins\\x.py").as_deref(),
            Some("plugins")
        );
        assert_eq!(owner("packages/api"), Some("api".to_owned()));
        assert_eq!(owner("packages/api2/app.py"), None);
        assert_eq!(owner("src/app.py"), None);
    }

    #[test]
    fn local_modules_cover_flat_src_and_member_roots() {
        let file = |path: &str| DiscoveredFile {
            path: path.to_owned(),
            kind: crate::sources::FileKind::Python,
            context: crate::sources::FileContext::Runtime,
        };
        let modules = local_modules(
            &[
                file("acme/core.py"),
                file("src/pkg/stub.pyi"),
                file("packages/api/src/api/views.py"),
                file("packages\\api\\plugins\\plug\\hooks.py"),
                file("README.md"),
            ],
            &[
                member("api", "packages/api"),
                member("plugins", "packages/api/plugins"),
            ],
        );
        for expected in [
            "acme",
            "acme.core",
            "pkg",
            "pkg.stub",
            "api",
            "api.views",
            "plug.hooks",
            "plugins.plug.hooks",
        ] {
            assert!(modules.contains(expected), "{expected}: {modules:?}");
        }
    }

    #[test]
    fn affix_stripped_drops_py_and_python_affixes() {
        fn stripped(name: &str) -> Vec<&str> {
            affix_stripped(name).collect()
        }
        assert_eq!(stripped("markdown-it-py"), vec!["markdown-it"]);
        assert_eq!(stripped("odfpy"), vec!["odf"]);
        assert_eq!(stripped("pydocket"), vec!["docket"]);
        assert_eq!(stripped("python-dateutil"), vec!["dateutil"]);
        assert!(stripped("py-spy").contains(&"spy"));
        assert_eq!(stripped("pyobjc-py"), vec!["objc-py", "pyobjc"]);
        assert_eq!(stripped("py"), Vec::<&str>::new());
        assert_eq!(stripped("requests"), Vec::<&str>::new());
    }

    #[test]
    fn loose_match_resolves_only_declared_names_as_maybe() {
        let declared = || ["markdown-it-py".to_owned(), "odfpy".to_owned()];
        let found = loose_declared_match("markdown-it", declared()).expect("markdown_it");
        assert_eq!(found.distribution.as_deref(), Some("markdown-it-py"));
        assert_eq!(found.confidence, ResolveConfidence::Maybe);
        assert_eq!(
            loose_declared_match("odf", declared()).and_then(|r| r.distribution),
            Some("odfpy".to_owned())
        );
        assert_eq!(loose_declared_match("docket", declared()), None);
        assert_eq!(loose_declared_match("markdown-it-py", declared()), None);
    }

    mod props {
        use proptest::prelude::*;

        use super::*;

        const PREFIXES: [&str; 3] = ["python-", "py-", "py"];
        const SUFFIXES: [&str; 3] = ["-python", "-py", "py"];

        /// Normalized names built from the letters that spell the affixes,
        /// so accidental affixes are common.
        fn name() -> impl Strategy<Value = String> {
            "[pythonab]{1,5}(-[pythonab]{1,4}){0,2}"
        }

        proptest! {
            /// Every stripped form is a proper, well-formed core of the name
            /// left by removing one known affix.
            #[test]
            fn affix_stripped_yields_well_formed_cores(name in name()) {
                for core in affix_stripped(&name) {
                    prop_assert!(!core.is_empty() && core != name);
                    prop_assert!(!core.starts_with('-') && !core.ends_with('-'), "{core}");
                    let prefixed = PREFIXES.iter().any(|affix| name == format!("{affix}{core}"));
                    let suffixed = SUFFIXES.iter().any(|affix| name == format!("{core}{affix}"));
                    prop_assert!(prefixed || suffixed, "{name} -> {core}");
                }
            }

            /// Adding an affix to a core is undone by `affix_stripped`,
            /// except where a longer prefix (`python-` over `py`) claims it.
            #[test]
            fn affix_stripped_recovers_the_core(core in name(), affix in 0usize..6) {
                let affixed = match affix {
                    0..3 => format!("{}{core}", PREFIXES[affix]),
                    _ => format!("{core}{}", SUFFIXES[affix - 3]),
                };
                prop_assume!(!(affix == 2 && core.starts_with("thon-")));
                prop_assert!(
                    affix_stripped(&affixed).any(|stripped| stripped == core),
                    "{affixed} should strip to {core}"
                );
            }

            /// The loose match is the first declared name one affix away from
            /// the import; it never returns an exact name or an undeclared one.
            #[test]
            fn loose_match_is_the_first_affixed_declared_name(
                root in name(),
                declared in prop::collection::vec(name(), 0..6),
            ) {
                let expected = declared
                    .iter()
                    .find(|name| affix_stripped(name).any(|core| core == root))
                    .cloned();
                let found = loose_declared_match(&root, declared);
                prop_assert_eq!(found.as_ref().and_then(|r| r.distribution.clone()), expected);
                if let Some(found) = found {
                    prop_assert_eq!(found.confidence, ResolveConfidence::Maybe);
                    prop_assert_ne!(found.distribution.as_deref(), Some(root.as_str()));
                }
            }
        }
    }

    #[test]
    fn scoped_declaration_covers_only_its_own_script_or_member() {
        let scoped = ScopedDeclarations {
            scripts: BTreeMap::from([(
                "scripts/tool.py".to_owned(),
                BTreeSet::from(["rich".to_owned()]),
            )]),
            members: BTreeMap::from([("api".to_owned(), BTreeSet::from(["foo-bar".to_owned()]))]),
            member_locks: BTreeMap::new(),
        };
        let found = |root: &str, file: &str, member: Option<&str>| {
            scoped_declaration(root, file, member, &scoped, ScopedMatch::Exact)
                .and_then(|r| r.distribution)
        };
        assert_eq!(
            found("rich", "scripts/tool.py", None).as_deref(),
            Some("rich")
        );
        assert_eq!(found("rich", "src/app.py", None), None);
        assert_eq!(
            found("Foo_Bar", "packages/api/x.py", Some("api")).as_deref(),
            Some("foo-bar")
        );
        assert_eq!(found("Foo_Bar", "packages/web/x.py", Some("web")), None);
        assert_eq!(
            found("foo_bar", "packages/api/x.py", Some("api")).as_deref(),
            Some("foo-bar")
        );
    }
}
