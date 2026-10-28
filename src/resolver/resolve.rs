//! Import resolution orchestration.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use crate::config::{ChokkinConfig, ResolvedWorkspaceMember};
use crate::graph::ModuleOrigin;
use crate::manifest::{LoadedManifest, normalize_distribution_name};
use crate::parser::{
    ImportContext, ImportRef, ParseSummary, ParsedModule, import_context_for_file,
};
use crate::plugins::{ModuleReference, SysPathImport};
use crate::sources::{DiscoveredSources, FileContext, LocalTrees, path_to_module};

use super::first_party::is_first_party_import;
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
/// `local` marks cross-member imports as first-party so workspace packages
/// do not become false missing-dependency findings.
/// `sys_path_imports` lets an importer resolve through its import's
/// `sys.path` edits.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "takes one output per earlier pipeline phase; a bundle struct would only rename them"
)]
pub fn resolve_imports_for_analysis(
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    parse: &ParseSummary,
    plugin_refs: &[ModuleReference],
    sys_path_imports: &[SysPathImport],
    local: &LocalTrees,
    script_targets: &BTreeMap<String, StdlibRange>,
    scoped: &ScopedDeclarations,
) -> ResolutionIndex {
    let stdlib = StdlibRange::for_project(
        config.target_version.as_ref(),
        manifest.metadata.requires_python.as_deref(),
    );

    let import_map = ImportMap::build(config).with_local_sources(local.path_imports().clone());
    let mut warnings = Vec::new();
    // The pytest search paths read only the sources and the parse; build
    // them beside the venv index and the local module sets.
    let (pytest_paths, (venv_index, local_modules, indexed_roots)) = crate::parallel::join(
        sources.files.len(),
        || {
            PytestImportPaths::build(sources).with_sys_path_hints(
                &sources.root.path,
                parse,
                sys_path_imports,
            )
        },
        || {
            (
                load_venv_index(&manifest.root, &mut warnings),
                local.modules(&sources.files),
                indexed_roots(sources),
            )
        },
    );
    let binary_resolutions = build_binary_map(config, &venv_index);
    let resolver = Resolver {
        sources,
        manifest,
        declared: DeclaredNames::new(manifest),
        local,
        owners: MemberOwners::new(local.members()),
        import_map: &import_map,
        venv_imports: &venv_index.imports,
        scoped,
        pytest_paths: &pytest_paths,
        local_modules: &local_modules,
        indexed_roots: &indexed_roots,
    };

    // Sites resolve per module on workers, each with its own root cache;
    // warnings then merge in module order, so they read as a sequential run.
    let per_module =
        crate::parallel::map_in_order_with(&parse.modules, RootCache::new, |cache, module| {
            let file_stdlib = script_targets.get(&module.path).copied().unwrap_or(stdlib);
            resolver.module_sites(module, file_stdlib, cache)
        });
    let mut imports = Vec::new();
    let mut merge = WarningMerge::default();
    for site in per_module.into_iter().flatten() {
        if site.quiet_unresolved {
            // A plugin name nothing resolves is a distribution missing from
            // the environment, not a broken import in this file; a loader
            // API may be handed a bad name on purpose (werkzeug's tests call
            // `import_string("os:foobar")`, #493).
            let mut site_warnings = Vec::new();
            merge.replay(site.warnings, &mut site_warnings);
            warnings.extend(
                site_warnings
                    .into_iter()
                    .filter(|warning| !matches!(warning, ResolveWarning::UnresolvedImport { .. })),
            );
        } else {
            merge.replay(site.warnings, &mut warnings);
        }
        imports.push(Arc::new(site.import));
    }

    // Plugin references resolve on workers too: airflow names 2.7k modules
    // by string, and each unresolved one walks every declaration (#1087).
    let per_reference =
        crate::parallel::map_in_order_with(plugin_refs, RootCache::new, |cache, reference| {
            let mut log = Vec::new();
            let import = resolver.site(
                &Site {
                    full_module: &reference.module,
                    imported: &reference.module,
                    file: &reference.origin.file,
                    line: reference.origin.line,
                    reference: Some(&reference.origin.label),
                    context: ImportContext::Runtime,
                    optional: false,
                    never_runs: false,
                    version_minors: None,
                    platform_guarded: false,
                    relative_level: 0,
                    stdlib,
                },
                cache,
                &mut log,
            );
            (import, log)
        });
    for (import, log) in per_reference {
        imports.push(Arc::new(import));
        merge.replay(log, &mut warnings);
    }

    let string_refs = resolver.string_refs(parse, stdlib);

    ResolutionIndex {
        imports,
        string_refs,
        warnings,
        transitive: manifest.lockfile.clone(),
        binary_resolutions,
        pytest_plugin_distributions: venv_index.pytest_plugins,
    }
}

/// Read-only inputs every import site resolves against, shared by the
/// resolve workers.
struct Resolver<'a> {
    sources: &'a DiscoveredSources,
    manifest: &'a LoadedManifest,
    declared: DeclaredNames<'a>,
    local: &'a LocalTrees,
    owners: MemberOwners<'a>,
    import_map: &'a ImportMap,
    venv_imports: &'a BTreeMap<String, Vec<String>>,
    scoped: &'a ScopedDeclarations,
    pytest_paths: &'a PytestImportPaths,
    local_modules: &'a std::collections::HashMap<String, bool>,
    indexed_roots: &'a BTreeSet<String>,
}

/// One import site: a statement, a dynamic import, a `pytest_plugins` entry,
/// or a plugin's module reference.
struct Site<'s> {
    full_module: &'s str,
    imported: &'s str,
    file: &'s str,
    line: Option<u32>,
    reference: Option<&'s str>,
    context: ImportContext,
    optional: bool,
    never_runs: bool,
    /// The Python 3 minors `sys.version_info` tests let the import run on.
    version_minors: Option<&'s [[u32; 2]]>,
    platform_guarded: bool,
    relative_level: u8,
    stdlib: StdlibRange,
}

/// A resolved site with the warnings it raised, not yet merged.
struct SiteOutcome {
    import: ResolvedImport,
    warnings: Vec<SiteWarning>,
    /// A name nothing resolves raises no unresolved import: a
    /// `pytest_plugins` entry or a loader API's argument.
    quiet_unresolved: bool,
}

/// A warning as one site raised it. Whether it reaches the report depends on
/// the sites before it in module order, which a worker cannot see.
enum SiteWarning {
    /// Raised resolving the root behind this cache key: reported at the key's
    /// first lookup in module order only, whichever worker computed it.
    Root((StdlibRange, String), Vec<ResolveWarning>),
    /// Raised by the site itself.
    Site(ResolveWarning),
}

/// Merges site warnings in module order as one sequential pass would have
/// raised them.
#[derive(Default)]
struct WarningMerge {
    reported_roots: BTreeSet<(StdlibRange, String)>,
}

impl WarningMerge {
    fn replay(&mut self, raised: Vec<SiteWarning>, target: &mut Vec<ResolveWarning>) {
        for warning in raised {
            match warning {
                SiteWarning::Root(key, warnings) => {
                    if self.reported_roots.insert(key) {
                        for warning in warnings {
                            report(warning, target);
                        }
                    }
                },
                SiteWarning::Site(warning) => report(warning, target),
            }
        }
    }
}

/// An ambiguity is reported once per target; an unresolved import per site.
fn report(warning: ResolveWarning, target: &mut Vec<ResolveWarning>) {
    if matches!(warning, ResolveWarning::AmbiguousImport { .. }) && target.contains(&warning) {
        return;
    }
    target.push(warning);
}

/// Root-manifest names normalized once per resolve: every import site
/// compares its root and candidates against them (#758).
struct DeclaredNames<'a> {
    /// Declared dependencies in declaration order; the affix step takes the
    /// first match.
    dependencies: Vec<String>,
    lookup: HashSet<String>,
    locked: &'a BTreeMap<String, Vec<String>>,
    /// Contextless requirements, then `[build-system].requires`.
    contextless: Vec<String>,
}

impl<'a> DeclaredNames<'a> {
    fn new(manifest: &'a LoadedManifest) -> Self {
        let dependencies: Vec<String> = manifest
            .dependencies
            .iter()
            .map(|dep| normalize_distribution_name(&dep.name))
            .collect();
        let build = manifest.metadata.build_requires.iter();
        Self {
            lookup: dependencies.iter().cloned().collect(),
            dependencies,
            locked: &manifest.lockfile.edges,
            contextless: manifest
                .sources
                .extra_requirements
                .iter()
                .cloned()
                .chain(build.map(|dep| normalize_distribution_name(&dep.name)))
                .collect(),
        }
    }

    fn declares(&self, distribution: &str) -> bool {
        self.lookup.contains(distribution)
    }

    fn declares_or_locks(&self, distribution: &str) -> bool {
        self.declares(distribution) || self.locked.contains_key(distribution)
    }
}

/// Keyed by (stdlib range, import root): stdlib membership depends on the
/// range, which differs between PEP 723 scripts.
/// Each entry keeps the warnings computing it raised, for [`WarningMerge`].
type RootCache = BTreeMap<(StdlibRange, String), (RootResolution, Vec<ResolveWarning>)>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootResolution {
    origin: ModuleOrigin,
    distribution: Option<String>,
    confidence: ResolveConfidence,
}

/// No Python `requires-python` allows runs the import: it is a fallback for
/// modules they all ship (#721), or a `sys.version_info` test rules out
/// every one of them (#997).
fn never_runs(import: &ImportRef, stdlib: StdlibRange) -> bool {
    let fallback_for = &import.fallback_for;
    (!fallback_for.is_empty() && fallback_for.iter().all(|root| stdlib.always_contains(root)))
        || import
            .version_minors
            .as_deref()
            .is_some_and(|runs| stdlib.excludes_minors(runs))
}

impl Resolver<'_> {
    /// Every import site in `module`, in the order a sequential pass visits
    /// them.
    fn module_sites(
        &self,
        module: &ParsedModule,
        stdlib: StdlibRange,
        cache: &mut RootCache,
    ) -> Vec<SiteOutcome> {
        let mut outcomes = Vec::new();
        let mut resolve = |site: &Site<'_>, quiet_unresolved: bool| {
            let mut warnings = Vec::new();
            let import = self.site(site, cache, &mut warnings);
            outcomes.push(SiteOutcome {
                import,
                warnings,
                quiet_unresolved,
            });
        };
        for import in &module.imports {
            if import.module.is_empty() {
                continue;
            }
            let imported = import.name.as_ref().map_or_else(
                || import.module.clone(),
                |name| format!("{}.{name}", import.module),
            );
            let never_runs = never_runs(import, stdlib);
            resolve(
                &Site {
                    full_module: &import.module,
                    imported: &imported,
                    file: &module.path,
                    line: Some(import.line),
                    reference: None,
                    context: import.context,
                    optional: import.optional,
                    never_runs,
                    version_minors: import.version_minors.as_deref(),
                    platform_guarded: import.platform_guarded,
                    relative_level: import.relative_level,
                    stdlib,
                },
                false,
            );
        }
        for dynamic in &module.dynamic_imports {
            resolve(
                &Site {
                    full_module: &dynamic.module,
                    imported: &dynamic.module,
                    file: &module.path,
                    line: Some(dynamic.line),
                    reference: None,
                    context: ImportContext::Runtime,
                    optional: dynamic.optional,
                    never_runs: false,
                    version_minors: None,
                    platform_guarded: dynamic.platform_guarded,
                    relative_level: 0,
                    stdlib,
                },
                dynamic.fetched,
            );
        }
        if module.pytest_plugins.is_empty() {
            return outcomes;
        }
        let plugin_context = self.plugin_context(module);
        for plugin in &module.pytest_plugins {
            resolve(
                &Site {
                    full_module: &plugin.module,
                    imported: &plugin.module,
                    file: &module.path,
                    line: Some(plugin.line),
                    reference: Some("pytest_plugins"),
                    context: plugin_context,
                    optional: false,
                    never_runs: false,
                    version_minors: None,
                    platform_guarded: false,
                    relative_level: 0,
                    stdlib,
                },
                true,
            );
        }
        outcomes
    }

    /// The dotted-name string literals of runtime modules that resolve to a
    /// third-party module (#1266), resolved as runtime imports of the module
    /// they name. A name is no import, so nothing it resolves to or fails to
    /// resolve to raises a warning; a first-party, stdlib or unresolved name
    /// is dropped, since it names no distribution.
    fn string_refs(&self, parse: &ParseSummary, stdlib: StdlibRange) -> Vec<ResolvedImport> {
        let runtime: HashSet<&str> = self
            .sources
            .files
            .iter()
            .filter(|file| file.context == FileContext::Runtime)
            .map(|file| file.path.as_str())
            .collect();
        let per_module =
            crate::parallel::map_in_order_with(&parse.modules, RootCache::new, |cache, module| {
                if !runtime.contains(module.path.as_str()) {
                    return Vec::new();
                }
                module
                    .module_refs
                    .iter()
                    .filter_map(|reference| {
                        let name = crate::parser::module_ref_name(&reference.text);
                        let import = self.site(
                            &Site {
                                full_module: name,
                                imported: name,
                                file: &module.path,
                                line: Some(reference.line),
                                reference: None,
                                context: ImportContext::Runtime,
                                optional: false,
                                never_runs: false,
                                version_minors: None,
                                platform_guarded: false,
                                relative_level: 0,
                                stdlib,
                            },
                            cache,
                            &mut Vec::new(),
                        );
                        (import.origin == ModuleOrigin::ThirdParty).then_some(import)
                    })
                    .collect()
            });
        per_module.into_iter().flatten().collect()
    }

    fn plugin_context(&self, module: &ParsedModule) -> ImportContext {
        self.sources
            .files
            .iter()
            .find(|file| file.path == module.path)
            .map_or(ImportContext::Test, |file| {
                import_context_for_file(file.context)
            })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one site's resolution steps in order; each step falls through to the next"
    )]
    fn site(
        &self,
        site: &Site<'_>,
        cache: &mut RootCache,
        warnings: &mut Vec<SiteWarning>,
    ) -> ResolvedImport {
        let &Site {
            full_module,
            imported,
            file,
            line,
            reference,
            context,
            optional,
            never_runs,
            version_minors,
            platform_guarded,
            relative_level,
            stdlib,
        } = site;
        let Self {
            sources,
            manifest,
            ref declared,
            local,
            ref owners,
            import_map,
            venv_imports,
            scoped,
            pytest_paths,
            local_modules,
            indexed_roots,
        } = *self;

        let root_name = import_root(full_module).to_owned();
        let workspace_member = owners.owner(file);
        let member = workspace_member.as_deref();
        let pick =
            |candidates: &[String]| site_candidate(candidates, file, member, scoped, declared);
        // A relative import only reaches within the importer's own package,
        // whatever its root shares a name with (#896).
        let site = if relative_level > 0 {
            Some(ModuleOrigin::FirstParty)
        } else {
            site_origin(&root_name, file, context, stdlib, pytest_paths)
        };
        let core = if let Some(origin) = site {
            RootResolution {
                origin,
                distribution: None,
                confidence: ResolveConfidence::Certain,
            }
        } else {
            let (core, raised) = cache.entry((stdlib, root_name.clone())).or_insert_with(|| {
                let mut raised = Vec::new();
                let core = resolve_import_root(
                    &root_name,
                    stdlib,
                    sources,
                    manifest,
                    declared,
                    local,
                    import_map,
                    venv_imports,
                    &mut raised,
                );
                (core, raised)
            });
            // Logged on every lookup, hit or miss: which site computes the entry
            // depends on the worker schedule; the first in module order reports.
            if !raised.is_empty() {
                warnings.push(SiteWarning::Root(
                    (stdlib, root_name.clone()),
                    raised.clone(),
                ));
            }
            let core = core.clone();
            match (core.origin, import_map.namespace_candidates(imported)) {
                // The cache holds the root's first candidate; the pick is per site.
                (ModuleOrigin::ThirdParty, None) => declared_portion(
                    imported, file, member, scoped, declared,
                )
                .unwrap_or_else(|| RootResolution {
                    distribution: root_candidates(&root_name, venv_imports, import_map)
                        .map_or(core.distribution, |(candidates, _)| pick(&candidates)),
                    ..core
                }),
                (ModuleOrigin::Stdlib, _) | (_, None) => core,
                // A first-party root may share its namespace with a distribution
                // (`poetry` and `poetry.core`); the local tree wins when it has the
                // module itself.
                (ModuleOrigin::FirstParty, Some((module, ..)))
                    if local_modules.contains_key(module) =>
                {
                    core
                },
                (_, Some((_, distributions, confidence))) => {
                    let mut raised = Vec::new();
                    let core = root_resolution_from_candidates(
                        &root_name,
                        &distributions,
                        Some(confidence),
                        &mut raised,
                    );
                    warnings.extend(raised.into_iter().map(SiteWarning::Site));
                    RootResolution {
                        distribution: pick(&distributions),
                        ..core
                    }
                },
            }
        };

        // An exact name in the file's own script block or member manifest beats an
        // affixed root declaration (`pyfoo` must not take a script's `foo`).
        let core = if core.origin == ModuleOrigin::Unknown {
            scoped_declaration(&root_name, file, member, scoped, ScopedMatch::Exact)
                .or_else(|| namespace_declared_match(imported, declared))
                .or_else(|| root_loose_match(&root_name, declared))
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
                .or_else(|| contextless_requirement_match(&root_name, declared))
                .or_else(|| runtime_provided(&root_name))
                // Last, so a declaration still takes a third-party namespace
                // a vendored root directory shares (#1114).
                .or_else(|| {
                    pytest_paths
                        .in_root_namespace(file, &root_name)
                        .then_some(RootResolution {
                            origin: ModuleOrigin::FirstParty,
                            distribution: None,
                            confidence: ResolveConfidence::Certain,
                        })
                })
                .unwrap_or(core)
        } else {
            core
        };

        if core.origin == ModuleOrigin::Unknown {
            warnings.push(SiteWarning::Site(ResolveWarning::UnresolvedImport {
                import: root_name.clone(),
                file: file.to_owned(),
                line,
                reference: reference.map(str::to_owned),
                context,
                // The stdlib table is Python 3's: a module only Python 2
                // runs may well be one of its standard modules (#1272).
                optional: optional || never_runs || version_minors.is_some_and(<[_]>::is_empty),
            }));
        }

        // The range is the root's: a member's own `requires-python` may allow
        // the Python a never-run import runs on, so its findings stay.
        let in_root = member.is_none();
        ResolvedImport {
            import_root: root_name,
            full_module: full_module.to_owned(),
            file: file.to_owned(),
            workspace_member,
            line,
            reference: reference.map(str::to_owned),
            context,
            optional,
            platform_guarded,
            never_runs: never_runs && in_root,
            origin: core.origin,
            distribution: core.distribution,
            confidence: core.confidence,
        }
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

#[expect(
    clippy::too_many_arguments,
    reason = "takes the resolver's per-run indexes and warning buffer its caller builds once"
)]
fn resolve_import_root(
    root_name: &str,
    stdlib: StdlibRange,
    sources: &DiscoveredSources,
    manifest: &LoadedManifest,
    declared: &DeclaredNames<'_>,
    local: &LocalTrees,
    import_map: &ImportMap,
    venv_imports: &BTreeMap<String, Vec<String>>,
    warnings: &mut Vec<ResolveWarning>,
) -> RootResolution {
    // A declared backport (`legacy-cgi` for `cgi`) provides the module on the
    // minors that dropped it, so the import is its use on any target, and
    // the other backports of that module do not make it ambiguous (#939).
    if stdlib.contains(root_name) || stdlib.ships_only_outside(root_name) {
        let backport =
            root_candidates(root_name, venv_imports, import_map).and_then(|(candidates, _)| {
                candidates.into_iter().find(|name| declared.declares(name))
            });
        if backport.is_some() {
            return RootResolution {
                origin: ModuleOrigin::ThirdParty,
                distribution: backport,
                confidence: ResolveConfidence::Certain,
            };
        }
    }

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

    if local.names_member(root_name) {
        return RootResolution {
            origin: ModuleOrigin::FirstParty,
            distribution: None,
            confidence: ResolveConfidence::Certain,
        };
    }

    if let Some((distributions, confidence)) = root_candidates(root_name, venv_imports, import_map)
    {
        // A declared candidate settles which variant provides the root, so
        // `opencv-python` beside `opencv-python-headless` is no ambiguity (#1139).
        let mut settled = Vec::new();
        let warnings = if distributions.iter().any(|name| declared.declares(name)) {
            &mut settled
        } else {
            warnings
        };
        return root_resolution_from_candidates(root_name, &distributions, confidence, warnings);
    }

    // No map names the root. A declared or locked distribution whose
    // normalized name matches (`openai`, or `Foo_Bar` for `import foo_bar`)
    // is the only evidence left; the spelling alone is not, or a local module
    // like `e2e_config` would pass as a missing `e2e-config` (#361).
    if let Some(resolution) = declared_match(normalize_distribution_name(root_name), declared) {
        return resolution;
    }

    // The affixed-name step runs per site in [`resolve_import_site`], after a
    // script or member's exact declaration has had its chance.
    RootResolution {
        origin: ModuleOrigin::Unknown,
        distribution: None,
        confidence: ResolveConfidence::Maybe,
    }
}

fn declared_match(distribution: String, declared: &DeclaredNames<'_>) -> Option<RootResolution> {
    declared
        .declares_or_locks(&distribution)
        .then_some(RootResolution {
            origin: ModuleOrigin::ThirdParty,
            distribution: Some(distribution),
            confidence: ResolveConfidence::Likely,
        })
}

/// A namespace package portion declared under its first two segments
/// (`sphinxcontrib-log-cabinet` for `sphinxcontrib.log_cabinet`, #809). The
/// shared root alone names no distribution.
fn namespace_declared_match(module: &str, declared: &DeclaredNames<'_>) -> Option<RootResolution> {
    let mut segments = module.split('.');
    let (namespace, portion) = (segments.next()?, segments.next()?);
    declared_match(
        normalize_distribution_name(&format!("{namespace}-{portion}")),
        declared,
    )
}

/// A namespace portion declared under its first two segments beats the
/// shared root's own distribution, however that was found (`optimum-intel`
/// for `optimum.intel` while `optimum` is only locked, #1022). The file's
/// script block, its member manifest, or the root must declare it; a locked
/// portion alone leaves the root's resolution, as the import may well be the
/// root's own subpackage.
fn declared_portion(
    module: &str,
    file: &str,
    member: Option<&str>,
    scoped: &ScopedDeclarations,
    declared: &DeclaredNames<'_>,
) -> Option<RootResolution> {
    let mut segments = module.split('.');
    let (namespace, portion) = (segments.next()?, segments.next()?);
    let distribution = normalize_distribution_name(&format!("{namespace}-{portion}"));
    let names_it =
        |sets: Option<&BTreeSet<String>>| sets.is_some_and(|names| names.contains(&distribution));
    (names_it(scoped.scripts.get(file))
        || names_it(member.and_then(|member| scoped.members.get(member)))
        || declared.declares(&distribution))
    .then_some(RootResolution {
        origin: ModuleOrigin::ThirdParty,
        distribution: Some(distribution),
        confidence: ResolveConfidence::Likely,
    })
}

/// Affixed counterpart of the declared-or-locked step in
/// [`resolve_import_root`] (`pyfoo` for `import foo`).
fn root_loose_match(root_name: &str, declared: &DeclaredNames<'_>) -> Option<RootResolution> {
    let locked = declared.locked.keys();
    loose_declared_match(
        &normalize_distribution_name(root_name),
        declared.dependencies.iter().chain(locked).cloned(),
    )
}

/// A name declared only in a requirements file whose context is unknown
/// (`requirements/tests.in`, #679) or in `[build-system].requires` (#720)
/// makes the root third-party, exactly or by affix. No distribution is
/// attached, so CHK003–CHK005 never judge the import against a context
/// neither source gives.
fn contextless_requirement_match(
    root_name: &str,
    declared: &DeclaredNames<'_>,
) -> Option<RootResolution> {
    let distribution = normalize_distribution_name(root_name);
    let confidence = if declared.contextless.contains(&distribution) {
        ResolveConfidence::Likely
    } else {
        loose_declared_match(&distribution, declared.contextless.iter().cloned())?.confidence
    };
    Some(RootResolution {
        origin: ModuleOrigin::ThirdParty,
        distribution: None,
        confidence,
    })
}

/// Modules the Pyodide runtime provides itself, not an installed
/// distribution (#722).
const RUNTIME_PROVIDED: [&str; 4] = ["_pyodide", "js", "pyodide", "pyodide_js"];

/// Last resort before CHK010: a root only a runtime provides is third-party
/// without a distribution, so a venv or map package of that name wins.
fn runtime_provided(root_name: &str) -> Option<RootResolution> {
    RUNTIME_PROVIDED
        .contains(&root_name)
        .then_some(RootResolution {
            origin: ModuleOrigin::ThirdParty,
            distribution: None,
            confidence: ResolveConfidence::Certain,
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
    declared: &DeclaredNames<'_>,
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
        &|name| declared.declares(name),
        &|name| in_member(&scoped.member_locks, name),
        &|name| declared.locked.contains_key(name),
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

    #[test]
    fn declared_names_are_normalized_once_in_declaration_order() {
        use crate::manifest::{
            DeclaredDependency, DependencyContext, DependencyOrigin, ProjectMetadata,
        };

        let dependency = |name: &str| DeclaredDependency {
            name: name.to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context: DependencyContext::Runtime,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(1),
                label: "project.dependencies[0]".to_owned(),
            },
            opaque: false,
            included_via: Vec::new(),
            unportable_key: None,
        };
        let mut manifest = LoadedManifest {
            root: crate::discovery::ProjectRoot {
                path: std::env::temp_dir(),
                marker: crate::discovery::RootMarker::PyProjectToml,
            },
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings::default(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: crate::manifest::LockfileGraph::default(),
            sources: crate::manifest::ManifestSources::default(),
            warnings: Vec::new(),
        };
        manifest
            .lockfile
            .edges
            .insert("locked-only".to_owned(), Vec::new());
        manifest.dependencies = vec![dependency("Zope.Interface"), dependency("py-Foo")];
        manifest.metadata.build_requires = vec![dependency("setuptools_scm")];
        manifest.sources.extra_requirements = BTreeSet::from(["tox".to_owned()]);

        let declared = DeclaredNames::new(&manifest);
        assert_eq!(declared.dependencies, ["zope-interface", "py-foo"]);
        assert_eq!(declared.contextless, ["tox", "setuptools-scm"]);
        assert!(declared.declares("zope-interface"));
        assert!(!declared.declares("locked-only"));
        assert!(declared.declares_or_locks("locked-only"));
        assert!(!declared.declares_or_locks("zope.interface"));
    }
}
