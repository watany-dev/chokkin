//! Symbol usage analysis orchestration (pipeline step 11).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::config::{Confidence, ProjectMode};
use crate::entry::EntryPlan;
use crate::graph::{FileId, ProjectGraph};
use crate::manifest::LoadedManifest;
use crate::parser::{ImportContext, ParseSummary, ParsedModule};
use crate::plugins::PluginHints;
use crate::reachability::{ModuleIndex, ReachabilityReport, module_and_parents};
use crate::resolver::is_first_party_import;
use crate::resolver::{ResolutionIndex, ResolveWarning};
use crate::rules::RuleContext;
use crate::rules::deps::file_context;
use crate::rules::types::{
    ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity, import_site,
    sort_candidates,
};
use crate::sources::{
    DiscoveredSources, FileContext, PublicSurface, in_examples_dir, member_ships, path_to_module,
};

use super::conventions::{
    config_star_symbols, convention_symbols, host_hook_symbols, is_codegen_file, scanned_files,
    settings_symbols,
};
use super::exports::{ReExport, collect_reexports, is_reexport_used};
use super::external::{
    collect_external_symbols, dynamically_fetched_classes, registered_by, string_named_symbols,
};
use super::graph::{ReferenceIndex, RegistryEntry, SymbolId, build_registry};
use super::public::{PublicApi, exports_reexport, is_public_module};

/// The reachable modules whose symbols form an API surface.
fn surface_modules<'m>(
    reachable_modules: &[&'m ParsedModule],
    sources: &DiscoveredSources,
    test_files: &HashSet<&str>,
    plugins: &PluginHints,
) -> Vec<&'m ParsedModule> {
    // Test code and root `tests/`-style packages are importable but not an
    // API surface: pytest calls their functions, so their symbols would all
    // read as unused. They still reference the symbols they import.
    let all_files: HashSet<&str> = sources
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let scanned_files = scanned_files(plugins);
    reachable_modules
        .iter()
        .copied()
        .filter(|module| {
            !sources.layout.in_local_package(&module.path)
                && !test_files.contains(module.path.as_str())
                && !is_codegen_file(&module.path, &all_files)
                && !scanned_files.contains(module.path.as_str())
        })
        .collect()
}

/// Analyze public symbol usage and unresolved imports (§12).
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "takes one output per earlier pipeline phase; a bundle struct would only rename them"
)]
pub fn analyze_with_context(
    context: &RuleContext<'_>,
    entry: &EntryPlan,
    plugins: &PluginHints,
    manifest: &LoadedManifest,
    member_surfaces: &[(String, PublicSurface)],
    production_tests: Option<&[ParsedModule]>,
) -> Vec<IssueCandidate> {
    let RuleContext {
        resolution,
        reachability,
        graph,
        sources,
        parse,
        surface,
    } = *context;
    let reachable = reachable_file_paths(graph, reachability);
    let reachable_modules: Vec<_> = parse
        .modules
        .iter()
        .filter(|module| reachable.contains(module.path.as_str()))
        .collect();
    let module_names = build_module_names(&reachable_modules, sources);
    let test_files = test_file_paths(sources, entry);
    let surface_modules = surface_modules(&reachable_modules, sources, &test_files, plugins);

    // The reference index is the costly part and reads only the reachable
    // modules; the registry and re-exports build beside it.
    let ((registry, reexports), reference_index) = crate::parallel::join(
        reachable_modules.len(),
        || {
            (
                build_registry(&surface_modules, &module_names),
                collect_reexports(&surface_modules, &module_names, &sources.layout),
            )
        },
        || ReferenceIndex::build(&reachable_modules, &module_names),
    );
    let public_api = PublicApi::build(
        &surface_modules,
        &module_names,
        production_api_references(production_tests, parse, &reachable, graph, sources),
    );
    let mut external_symbols =
        collect_external_symbols(&registry, entry, plugins, &module_names, &sources.layout);
    external_symbols.extend(convention_symbols(plugins, &surface_modules, &module_names));
    external_symbols.extend(dynamically_fetched_classes(&registry, &reachable_modules));
    external_symbols.extend(registered_by(&registry, &reachable_modules, &module_names));
    external_symbols.extend(string_named_symbols(
        &reachable_modules,
        &module_names,
        &test_files,
    ));
    external_symbols.extend(settings_symbols(plugins, &registry));
    external_symbols.extend(host_hook_symbols(
        &registry,
        &reexports,
        &reachable_modules,
        entry,
        plugins,
        reachability,
    ));
    external_symbols.extend(config_star_symbols(
        plugins,
        &registry,
        &reachable_modules,
        &module_names,
    ));

    let wheels = Wheels {
        entry,
        root: surface,
        members: member_surfaces,
    };
    let mut candidates = detect_unused_exports(
        &registry,
        &reference_index,
        &external_symbols,
        &public_api,
        &wheels,
    );
    candidates.extend(detect_unused_reexports(
        &reexports,
        &reference_index,
        &external_symbols,
        &wheels,
    ));
    candidates.extend(detect_unresolved_imports(
        resolution, &reachable, manifest, sources,
    ));

    sort_candidates(&mut candidates);
    candidates
}

struct Wheels<'a> {
    entry: &'a EntryPlan,
    /// The root wheel's files; `None` when it declares no wheel targets.
    root: Option<&'a PublicSurface>,
    /// Each member's root-relative directory with its member-relative surface.
    members: &'a [(String, PublicSurface)],
}

impl Wheels<'_> {
    /// Whether a wheel ships root-relative `path`: its innermost member's own
    /// wheel, else the root's. A shipped `__all__` is public API whatever the
    /// mode, since a workspace root with an app signal and a member with its
    /// own CLI resolve to app (#678, #727).
    fn ships(&self, path: &str) -> bool {
        member_ships(self.members, path)
            .unwrap_or_else(|| self.root.as_ref().is_some_and(|root| root.contains(path)))
    }
}

fn reachable_file_paths<'g>(
    graph: &'g ProjectGraph,
    reachability: &ReachabilityReport,
) -> HashSet<&'g str> {
    reachability
        .reachable
        .iter()
        .filter_map(|file_id| graph.file(*file_id).map(|node| node.path.as_str()))
        .collect()
}

/// Files whose context is `Test`, by path or because a plugin rooted them as
/// tests (pytest `testpaths` with custom `python_files`).
fn test_file_paths<'a>(sources: &'a DiscoveredSources, entry: &'a EntryPlan) -> HashSet<&'a str> {
    let by_path = sources
        .files
        .iter()
        .filter(|file| file.context == FileContext::Test)
        .map(|file| file.path.as_str());
    let by_root = entry
        .roots
        .iter()
        .filter(|root| root.context == FileContext::Test)
        .map(|root| root.path.as_str());
    by_path.chain(by_root).collect()
}

/// `--production` drops the tests and leaves the public modules only they
/// reached unreachable, yet both still show which public-module names an
/// outside caller uses (#588). An orphaned module, which the tests do not
/// reach either, is dead code, not a caller (#950); nor is a private one.
fn production_api_references(
    production_tests: Option<&[ParsedModule]>,
    parse: &ParseSummary,
    reachable: &HashSet<&str>,
    graph: &ProjectGraph,
    sources: &DiscoveredSources,
) -> ReferenceIndex {
    let Some(tests) = production_tests else {
        return ReferenceIndex::default();
    };
    let all: Vec<_> = tests.iter().chain(&parse.modules).collect();
    let names = build_module_names(&all, sources);
    let reached_public = reached_from_tests(tests, parse, reachable, graph, sources)
        .into_iter()
        .filter(|module| {
            names
                .get(module.path.as_str())
                .is_some_and(|name| is_public_module(name))
        });
    let evidence: Vec<_> = tests.iter().chain(reached_public).collect();
    ReferenceIndex::build(&evidence, &names)
}

/// The modules left unreachable that the tests reach through them. A module
/// the tests reach only through an edge `imported_files` does not read (a
/// plugin reference, a framework glob) is left out as an orphan is, so its
/// calls clear no CHK006 and `--production` reports what the default run
/// reports, or more.
fn reached_from_tests<'a>(
    tests: &'a [ParsedModule],
    parse: &'a ParseSummary,
    reachable: &HashSet<&str>,
    graph: &ProjectGraph,
    sources: &DiscoveredSources,
) -> Vec<&'a ParsedModule> {
    let index = ModuleIndex::build(graph, sources);
    let by_path: HashMap<&str, &ParsedModule> = parse
        .modules
        .iter()
        .filter(|module| !reachable.contains(module.path.as_str()))
        .map(|module| (module.path.as_str(), module))
        .collect();
    let mut seen = HashSet::new();
    let mut reached = Vec::new();
    let mut queue: Vec<&ParsedModule> = tests.iter().collect();
    while let Some(module) = queue.pop() {
        for file in imported_files(module, &index) {
            let Some(target) = graph
                .file(file)
                .and_then(|node| by_path.get(node.path.as_str()))
            else {
                continue;
            };
            if seen.insert(target.path.as_str()) {
                reached.push(*target);
                queue.push(*target);
            }
        }
    }
    reached
}

fn imported_files(module: &ParsedModule, index: &ModuleIndex) -> Vec<FileId> {
    let from = module.path.as_str();
    let dotted = module
        .imports
        .iter()
        .map(|import| import.module.as_str())
        .chain(
            module
                .dynamic_imports
                .iter()
                .chain(&module.pytest_plugins)
                .map(|import| import.module.as_str()),
        )
        .filter(|name| !name.is_empty());
    let mut files: Vec<_> = dotted
        .flat_map(module_and_parents)
        .filter_map(|name| index.resolve_from(from, name))
        .collect();
    files.extend(
        module
            .imports
            .iter()
            .filter(|import| !import.module.is_empty())
            .filter_map(|import| Some(format!("{}.{}", import.module, import.name.as_deref()?)))
            .filter_map(|submodule| index.resolve_from(from, &submodule)),
    );
    files.extend(
        module
            .dynamic_import_prefixes
            .iter()
            .flat_map(|prefix| index.under(&prefix.module))
            .map(|(_, file)| file),
    );
    files
}

fn build_module_names<'a>(
    modules: &[&'a ParsedModule],
    sources: &DiscoveredSources,
) -> HashMap<&'a str, String> {
    let mut names = HashMap::new();
    for module in modules {
        if let Some(name) = path_to_module(&module.path, &sources.layout) {
            names.insert(module.path.as_str(), name);
        }
    }
    names
}

fn detect_unused_exports(
    registry: &[RegistryEntry],
    references: &ReferenceIndex,
    external_symbols: &indexmap::IndexSet<SymbolId>,
    public_api: &PublicApi,
    wheels: &Wheels<'_>,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for entry in registry {
        if external_symbols.contains(&entry.id) {
            continue;
        }
        if references.is_externally_referenced(&entry.id) {
            continue;
        }
        if entry.in_all && wheels.ships(&entry.path) {
            continue;
        }
        // A library symbol the wheel does not ship has no outside caller (R-05).
        let mode = wheels.entry.surface_mode(&entry.path, wheels.root);
        // A library cannot make a name private that its own module reads:
        // it is a TypeVar, an alias, or a type reached through an attribute (#540).
        // Without `export`, an app's top-level name outside `__all__` is a plain
        // declaration, which knip's `exports` does not report either (#564).
        let skip = match mode {
            ProjectMode::Library => entry.def.used_in_module || public_api.exports_symbol(entry),
            ProjectMode::App | ProjectMode::Auto => entry.def.used_in_module && !entry.in_all,
        };
        if skip {
            continue;
        }

        let (severity, confidence) = unused_export_severity(mode, entry.in_all);
        // A sample under `examples/` is copied, not imported, as its CHK001
        // says; a `literalinclude` entry made it reachable (#1255).
        let example = in_examples_dir(&entry.path);
        let severity = if example { Severity::Info } else { severity };
        candidates.push(IssueCandidate {
            rule: RuleId::Chk006,
            subject: IssueSubject::Symbol {
                module: entry.id.module.clone(),
                name: entry.id.name.clone(),
            },
            severity,
            confidence,
            message: format!(
                "public {} `{}` in `{}` is not referenced from outside the module",
                symbol_kind_label(entry.def.kind),
                entry.id.name,
                entry.id.module
            ),
            workspace_member: None,
            origins: vec![Origin::Import {
                file: entry.path.clone(),
                line: Some(entry.def.line),
                module: entry.id.module.clone(),
            }],
            explain: ExplainData {
                summary: format!(
                    "{}.{} is a public symbol with no external references",
                    entry.id.module, entry.id.name
                ),
                details: vec![
                    "`from … import name` and `module.name` access on imported modules are tracked"
                        .to_owned(),
                    "decorated handlers, fixtures, and entry targets are excluded".to_owned(),
                ]
                .into_iter()
                .chain(example.then(|| "the file is under an `examples/` directory".to_owned()))
                .collect(),
            },
        });
    }

    candidates
}

fn detect_unused_reexports(
    reexports: &[ReExport],
    references: &ReferenceIndex,
    external_symbols: &indexmap::IndexSet<SymbolId>,
    wheels: &Wheels<'_>,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for reexport in reexports {
        if is_reexport_used(reexport, references, external_symbols) {
            continue;
        }
        let mode = wheels.entry.mode_for(&reexport.path);
        if mode == ProjectMode::Library && exports_reexport(reexport) {
            continue;
        }
        if reexport.declared_public && wheels.ships(&reexport.path) {
            continue;
        }
        let (severity, confidence) = unused_reexport_severity(mode);
        candidates.push(IssueCandidate {
            rule: RuleId::Chk007,
            subject: IssueSubject::Symbol {
                module: reexport.package_module.clone(),
                name: reexport.name.clone(),
            },
            severity,
            confidence,
            message: format!(
                "re-export `{}` in `{}` is not imported from the package or used internally",
                reexport.name, reexport.package_module
            ),
            workspace_member: None,
            origins: vec![Origin::Import {
                file: reexport.path.clone(),
                line: Some(reexport.line),
                module: reexport.source_module.clone(),
            }],
            explain: ExplainData {
                summary: format!(
                    "{} re-exports {} but nothing imports it from the package",
                    reexport.package_module, reexport.name
                ),
                details: vec![format!("resolved from module `{}`", reexport.source_module)],
            },
        });
    }

    candidates
}

fn unused_export_severity(mode: ProjectMode, in_all: bool) -> (Severity, Confidence) {
    let confidence = if in_all {
        Confidence::Certain
    } else {
        Confidence::Likely
    };
    let severity = match mode {
        ProjectMode::Library => Severity::Info,
        ProjectMode::App | ProjectMode::Auto => Severity::Warning,
    };
    (severity, confidence)
}

fn unused_reexport_severity(mode: ProjectMode) -> (Severity, Confidence) {
    let severity = match mode {
        ProjectMode::Library => Severity::Info,
        ProjectMode::App | ProjectMode::Auto => Severity::Warning,
    };
    (severity, Confidence::Likely)
}

fn detect_unresolved_imports(
    resolution: &ResolutionIndex,
    reachable: &HashSet<&str>,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
) -> Vec<IssueCandidate> {
    // One issue per (file, module): lazy imports inside functions repeat the
    // same unresolved module many times in one file and bury the rest.
    let mut sites: HashMap<(&str, &str), Vec<UnresolvedSite<'_>>> = HashMap::new();
    for warning in &resolution.warnings {
        let ResolveWarning::UnresolvedImport {
            import,
            file,
            line,
            reference,
            context,
            optional,
        } = warning
        else {
            continue;
        };
        if !reachable.contains(file.as_str()) {
            continue;
        }
        let guarded = *context == ImportContext::Type || *optional;
        sites
            .entry((file.as_str(), import.as_str()))
            .or_default()
            .push(UnresolvedSite {
                line: *line,
                guarded,
                reference: reference.as_deref(),
            });
    }

    sites
        .into_iter()
        .filter_map(|((file, import), sites)| {
            unresolved_import_candidate(file, import, &sites, manifest, sources)
        })
        .collect()
}

struct UnresolvedSite<'a> {
    line: Option<u32>,
    /// `TYPE_CHECKING` or optional: the import never fails.
    guarded: bool,
    /// Config setting naming the module, `None` for an `import` statement.
    reference: Option<&'a str>,
}

// Anchor at a site that can fail, preferring one with a position.
fn anchor_site<'s>(sites: &'s [UnresolvedSite<'_>]) -> Option<&'s UnresolvedSite<'s>> {
    sites
        .iter()
        .min_by_key(|site| (site.guarded, site.line.is_none(), site.line))
}

fn unresolved_import_candidate(
    file: &str,
    import: &str,
    sites: &[UnresolvedSite<'_>],
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
) -> Option<IssueCandidate> {
    let anchor = anchor_site(sites)?;
    let line = anchor.line;
    let guarded_only = sites.iter().all(|site| site.guarded);
    let others = sites
        .iter()
        .map(|site| site.line)
        .filter(|other| *other != line)
        .collect::<BTreeSet<_>>();
    let first_party = is_first_party_import(import, &sources.layout, &manifest.metadata);
    let site = import_site(file, line);
    // A config string (Sphinx `extensions`, `INSTALLED_APPS`) is no `import`
    // statement; say which setting names the module (#830).
    let subject = anchor.reference.map_or_else(
        || format!("import `{import}`"),
        |label| format!("`{label}` entry `{import}`"),
    );
    let message = if first_party {
        format!("{subject} in `{site}` does not resolve to a first-party module")
    } else {
        format!("{subject} in `{site}` could not be resolved")
    };
    let mut details = vec![
        "check for typos in first-party module names".to_owned(),
        "third-party packages may be missing from dependency declarations".to_owned(),
    ];
    if let Some(label) = anchor.reference {
        details.insert(0, format!("`{label}` entry at {site}"));
    }
    let numbered = others
        .iter()
        .flatten()
        .map(u32::to_string)
        .collect::<Vec<_>>();
    if !numbered.is_empty() {
        details.push(format!("also imported at lines {}", numbered.join(", ")));
    }

    Some(IssueCandidate {
        rule: RuleId::Chk010,
        subject: IssueSubject::Import {
            module: import.to_owned(),
            file: file.to_owned(),
            line,
            distribution: None,
        },
        // A `TYPE_CHECKING` import never runs and an optional one falls back
        // when missing; docs / examples / scripts run outside the installed
        // package. All stay reported for typos (#584, #654, #694).
        severity: if guarded_only
            || matches!(
                file_context(file, sources),
                FileContext::Docs | FileContext::Dev
            ) {
            Severity::Info
        } else {
            Severity::Warning
        },
        confidence: Confidence::Likely,
        message,
        workspace_member: None,
        // Every site, anchor first, so an inline ignore must cover them all.
        // A site without a line takes no directive, so it keeps the merged
        // issue from being ignored inline even when it is not the anchor.
        origins: std::iter::once(line)
            .chain(others)
            .map(|line| Origin::Import {
                file: file.to_owned(),
                line,
                module: import.to_owned(),
            })
            .collect(),
        explain: ExplainData {
            summary: if first_party {
                format!("`{import}` looks like a first-party import but is unresolved")
            } else {
                format!("`{import}` is not stdlib, first-party, or a known third-party package")
            },
            details,
        },
    })
}

fn symbol_kind_label(kind: crate::parser::SymbolKind) -> &'static str {
    match kind {
        crate::parser::SymbolKind::Function => "function",
        crate::parser::SymbolKind::Class => "class",
        crate::parser::SymbolKind::Variable => "constant",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor_line(sites: &[(Option<u32>, bool)]) -> Option<u32> {
        let sites = sites
            .iter()
            .map(|&(line, guarded)| UnresolvedSite {
                line,
                guarded,
                reference: None,
            })
            .collect::<Vec<_>>();
        anchor_site(&sites).expect("anchor").line
    }

    #[test]
    fn anchor_prefers_unguarded_then_positioned_then_earliest_line() {
        assert!(anchor_site(&[]).is_none());
        assert_eq!(anchor_line(&[(Some(4), true), (Some(8), false)]), Some(8));
        assert_eq!(anchor_line(&[(None, false), (Some(8), false)]), Some(8));
        assert_eq!(anchor_line(&[(Some(14), false), (Some(8), false)]), Some(8));
        assert_eq!(anchor_line(&[(None, false), (Some(4), true)]), None);
    }

    fn sources(files: &[(&str, FileContext)]) -> DiscoveredSources {
        let root = crate::discovery::ProjectRoot {
            path: std::env::temp_dir(),
            marker: crate::discovery::RootMarker::PyProjectToml,
        };
        DiscoveredSources {
            root,
            layout: crate::sources::LayoutInfo::default(),
            effective_globs: Vec::new(),
            files: files
                .iter()
                .map(|&(path, context)| crate::sources::DiscoveredFile {
                    path: path.to_owned(),
                    kind: crate::sources::FileKind::Python,
                    context,
                })
                .collect(),
            warnings: Vec::new(),
        }
    }

    fn manifest(sources: &DiscoveredSources) -> LoadedManifest {
        LoadedManifest {
            root: sources.root.clone(),
            metadata: crate::manifest::ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings::default(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: crate::manifest::LockfileGraph::default(),
            sources: crate::manifest::ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    const FILE: &str = "pkg/app.py";

    fn site_lists() -> Vec<Vec<(Option<u32>, bool)>> {
        let sites = [None, Some(1), Some(2)]
            .into_iter()
            .flat_map(|line| [(line, false), (line, true)]);
        let (mut lists, mut shorter) = (Vec::new(), vec![Vec::new()]);
        for _ in 0..3 {
            shorter = shorter
                .iter()
                .flat_map(|list: &Vec<_>| {
                    sites
                        .clone()
                        .map(|site| [list.as_slice(), &[site]].concat())
                })
                .collect();
            lists.extend(shorter.clone());
        }
        lists
    }

    fn ignored_inline(lines: &[u32]) -> crate::rules::ignore::IgnoreMatcher {
        let parse = ParseSummary {
            modules: vec![ParsedModule {
                path: FILE.to_owned(),
                ignores: lines
                    .iter()
                    .map(|&line| crate::parser::IgnoreDirective {
                        file_level: false,
                        codes: vec!["CHK010".to_owned()],
                        line,
                    })
                    .collect(),
                ..ParsedModule::default()
            }],
        };
        crate::rules::ignore::IgnoreMatcher::build(
            &crate::config::default_config(),
            &parse,
            &ResolutionIndex::default(),
        )
    }

    /// §12 CHK010 aggregation against a reference model, over every list of
    /// up to three sites (line 1, 2 or none; guarded or not) of one module,
    /// in each file context, with `# chokkin: ignore[CHK010]` on any subset
    /// of the lines: the merged severity is the highest a site alone would
    /// get, the anchor is the first line that can fail, and the inline
    /// ignore holds exactly when every site sits on an ignored line (#860).
    #[test]
    fn unresolved_imports_merge_as_the_sites_would_alone() {
        use crate::rules::types::SuppressReason;

        let contexts = [
            FileContext::Runtime,
            FileContext::Test,
            FileContext::Docs,
            FileContext::Dev,
        ];
        let ignored_sets: [&[u32]; 4] = [&[], &[1], &[2], &[1, 2]];
        for list in &site_lists() {
            for context in contexts {
                let sources = sources(&[(FILE, context)]);
                let manifest = manifest(&sources);
                let sites = list
                    .iter()
                    .map(|&(line, guarded)| UnresolvedSite {
                        line,
                        guarded,
                        reference: None,
                    })
                    .collect::<Vec<_>>();
                let candidate =
                    unresolved_import_candidate(FILE, "missing", &sites, &manifest, &sources)
                        .expect("candidate");

                let alone = |guarded: bool| {
                    if guarded || matches!(context, FileContext::Docs | FileContext::Dev) {
                        Severity::Info
                    } else {
                        Severity::Warning
                    }
                };
                let expected_severity = list
                    .iter()
                    .map(|&(_, guarded)| alone(guarded))
                    // `Error` sorts first: the strongest severity is the least.
                    .min()
                    .expect("sites");
                assert_eq!(
                    candidate.severity, expected_severity,
                    "{list:?} {context:?}"
                );

                let can_fail = list.iter().any(|&(_, guarded)| !guarded);
                let expected_anchor = list
                    .iter()
                    .filter(|&&(_, guarded)| !can_fail || !guarded)
                    .filter_map(|&(line, _)| line)
                    .min();
                let IssueSubject::Import { line, .. } = &candidate.subject else {
                    panic!("import subject");
                };
                assert_eq!(*line, expected_anchor, "{list:?}");

                let origin_lines = candidate
                    .origins
                    .iter()
                    .map(|origin| match origin {
                        Origin::Import { line, .. } => *line,
                        other => panic!("{other:?}"),
                    })
                    .collect::<Vec<_>>();
                assert_eq!(origin_lines.first(), Some(&expected_anchor), "{list:?}");
                assert_eq!(
                    origin_lines.iter().copied().collect::<BTreeSet<_>>(),
                    list.iter().map(|&(line, _)| line).collect::<BTreeSet<_>>(),
                    "{list:?}"
                );

                for ignored in ignored_sets {
                    let expected = list
                        .iter()
                        .all(|&(line, _)| line.is_some_and(|line| ignored.contains(&line)))
                        .then_some(SuppressReason::Inline);
                    assert_eq!(
                        ignored_inline(ignored).matches_candidate(&candidate),
                        expected,
                        "{list:?} ignored at {ignored:?}"
                    );
                }
            }
        }
    }
}
