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
    ResolutionIndex, ResolveConfidence, ResolveWarning, ResolvedImport, import_root,
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
    resolve_imports_for_analysis(
        config,
        manifest,
        sources,
        parse,
        plugin_refs,
        workspace_members,
        &BTreeMap::new(),
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

/// [`resolve_imports`] with per-file stdlib ranges for PEP 723 scripts, also
/// resolving an unmapped root through the script block or member manifest
/// owning the file.
///
/// A script's `requires-python` decides which modules are stdlib for the
/// imports in that file; every other file uses the project range.
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
    let pytest_paths = PytestImportPaths::build(sources);
    let local_modules = local_modules(&sources.files, workspace_members);

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
                &local_modules,
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
                &local_modules,
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
            &local_modules,
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
    local_modules: &BTreeSet<String>,
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
            (ModuleOrigin::FirstParty, Some((module, ..))) if local_modules.contains(module) => {
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
    let declared = || {
        manifest
            .dependencies
            .iter()
            .map(|dep| normalize_distribution_name(&dep.name))
    };
    if declared().any(|dep| dep == distribution)
        || manifest.lockfile.edges.contains_key(&distribution)
    {
        return RootResolution {
            origin: ModuleOrigin::ThirdParty,
            distribution: Some(distribution),
            confidence: ResolveConfidence::Likely,
        };
    }
    let locked = manifest.lockfile.edges.keys().cloned();
    if let Some(resolution) = loose_declared_match(&distribution, declared().chain(locked)) {
        return resolution;
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
    let declared = || {
        scoped
            .scripts
            .get(file)
            .into_iter()
            .chain(workspace_member.and_then(|member| scoped.members.get(member)))
            .flatten()
    };
    if declared().any(|name| *name == distribution) {
        return Some(RootResolution {
            origin: ModuleOrigin::ThirdParty,
            distribution: Some(distribution),
            confidence: ResolveConfidence::Likely,
        });
    }
    loose_declared_match(&distribution, declared().cloned())
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
    let roots: Vec<String> = ["", "src/"]
        .into_iter()
        .map(str::to_owned)
        .chain(
            workspace_members
                .iter()
                .flat_map(|member| [format!("{}/", member.path), format!("{}/src/", member.path)]),
        )
        .collect();
    let mut modules = BTreeSet::new();
    for file in files {
        let file = file.path.replace('\\', "/");
        for rest in roots
            .iter()
            .filter_map(|root| file.strip_prefix(root.as_str()))
        {
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

fn workspace_member_for_file(
    file: &str,
    workspace_members: &[ResolvedWorkspaceMember],
) -> Option<String> {
    let normalized = file.replace('\\', "/");
    workspace_members
        .iter()
        .filter(|member| {
            normalized
                .strip_prefix(member.path.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
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
        let owner = |file: &str| workspace_member_for_file(file, &members);
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
                file("README.md"),
            ],
            &[member("api", "packages/api")],
        );
        for expected in ["acme", "acme.core", "pkg", "pkg.stub", "api", "api.views"] {
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

    #[test]
    fn scoped_declaration_covers_only_its_own_script_or_member() {
        let scoped = ScopedDeclarations {
            scripts: BTreeMap::from([(
                "scripts/tool.py".to_owned(),
                BTreeSet::from(["rich".to_owned()]),
            )]),
            members: BTreeMap::from([("api".to_owned(), BTreeSet::from(["foo-bar".to_owned()]))]),
        };
        let found = |root: &str, file: &str, member: Option<&str>| {
            scoped_declaration(root, file, member, &scoped).and_then(|r| r.distribution)
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
