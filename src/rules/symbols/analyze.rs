//! Symbol usage analysis orchestration (pipeline step 11).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::config::{Confidence, ProjectMode};
use crate::entry::EntryPlan;
use crate::graph::ProjectGraph;
use crate::manifest::LoadedManifest;
use crate::parser::{ImportContext, ParseSummary, ParsedModule};
use crate::plugins::PluginHints;
use crate::reachability::ReachabilityReport;
use crate::resolver::is_first_party_import;
use crate::resolver::{ResolutionIndex, ResolveWarning};
use crate::rules::RuleContext;
use crate::rules::deps::file_context;
use crate::rules::types::{
    ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity, sort_candidates,
};
use crate::sources::{DiscoveredSources, FileContext, PublicSurface, member_ships, path_to_module};

use super::conventions::{alembic_script_files, alembic_symbols, is_codegen_file};
use super::exports::{ReExport, collect_reexports, is_reexport_used};
use super::external::collect_external_symbols;
use super::graph::{ReferenceIndex, RegistryEntry, SymbolId, build_registry};
use super::public::{PublicApi, exports_reexport, is_public_module};

/// Analyze public symbol usage and unresolved imports (§12).
#[must_use]
#[allow(clippy::too_many_arguments)]
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
    } = *context;
    let reachable = reachable_file_paths(graph, reachability);
    let reachable_modules: Vec<_> = parse
        .modules
        .iter()
        .filter(|module| reachable.contains(module.path.as_str()))
        .collect();
    let module_names = build_module_names(&reachable_modules, sources);
    // Test code and root `tests/`-style packages are importable but not an
    // API surface: pytest calls their functions, so their symbols would all
    // read as unused. They still reference the symbols they import.
    let test_files = test_file_paths(sources, entry);
    let all_files: HashSet<&str> = sources
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let alembic_files = alembic_script_files(plugins);
    let surface_modules: Vec<_> = reachable_modules
        .iter()
        .copied()
        .filter(|module| {
            !sources.layout.in_local_package(&module.path)
                && !test_files.contains(module.path.as_str())
                && !is_codegen_file(&module.path, &all_files)
                && !alembic_files.contains(module.path.as_str())
        })
        .collect();

    let registry = build_registry(&surface_modules, &module_names);
    let reference_index = ReferenceIndex::build(&reachable_modules, &module_names);
    let reexports = collect_reexports(&surface_modules, &module_names, &sources.layout);
    let public_api = PublicApi::build(
        &surface_modules,
        &module_names,
        production_api_references(production_tests, parse, &reachable, sources),
    );
    let mut external_symbols =
        collect_external_symbols(&registry, entry, plugins, &module_names, &sources.layout);
    external_symbols.extend(alembic_symbols(plugins, &surface_modules, &module_names));

    let surface = PublicSurface::resolve(manifest.metadata.wheel_targets.as_ref(), &sources.files);
    // A library member ships its own wheel, so its files are judged as a
    // library whatever the root mode is, and the root's wheel surface does
    // not apply to them (#515).
    let mode_for = |path: &str| {
        if entry.in_library_member(path) {
            ProjectMode::Library
        } else {
            entry.mode
        }
    };
    // A library symbol the wheel does not ship has no outside caller (R-05).
    let export_mode = |path: &str| {
        if !entry.in_library_member(path)
            && surface
                .as_ref()
                .is_some_and(|surface| !surface.contains(path))
        {
            ProjectMode::App
        } else {
            mode_for(path)
        }
    };
    let shipped = |path: &str| is_shipped(surface.as_ref(), member_surfaces, path);
    let mut candidates = detect_unused_exports(
        &registry,
        &reference_index,
        &external_symbols,
        &public_api,
        export_mode,
    );
    candidates.extend(detect_unused_reexports(
        &reexports,
        &reference_index,
        mode_for,
        shipped,
    ));
    candidates.extend(detect_unresolved_imports(
        resolution, &reachable, manifest, sources,
    ));

    sort_candidates(&mut candidates);
    candidates
}

/// Whether a wheel ships root-relative `path`: its innermost member's own
/// wheel, else the root's. A shipped `__all__` is public API whatever the
/// mode, since workspace roots and members with their own CLI resolve to app
/// (#678).
fn is_shipped(
    root: Option<&PublicSurface>,
    members: &[(String, PublicSurface)],
    path: &str,
) -> bool {
    member_ships(members, path).unwrap_or_else(|| root.is_some_and(|root| root.contains(path)))
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
        .map(|root| root.spec.path.as_str());
    by_path.chain(by_root).collect()
}

/// `--production` drops the tests and leaves the public modules only they
/// reached unreachable, yet both still show which public-module names an
/// outside caller uses (#588). An orphaned private module is dead code, not a
/// caller.
fn production_api_references(
    production_tests: Option<&[ParsedModule]>,
    parse: &ParseSummary,
    reachable: &HashSet<&str>,
    sources: &DiscoveredSources,
) -> ReferenceIndex {
    let Some(tests) = production_tests else {
        return ReferenceIndex::default();
    };
    let all: Vec<_> = tests.iter().chain(&parse.modules).collect();
    let names = build_module_names(&all, sources);
    let unreachable_public = parse.modules.iter().filter(|module| {
        !reachable.contains(module.path.as_str())
            && names
                .get(module.path.as_str())
                .is_some_and(|name| is_public_module(name))
    });
    let evidence: Vec<_> = tests.iter().chain(unreachable_public).collect();
    ReferenceIndex::build(&evidence, &names)
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
    mode_for: impl Fn(&str) -> ProjectMode,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for entry in registry {
        if external_symbols.contains(&entry.id) {
            continue;
        }
        if references.is_externally_referenced(&entry.id) {
            continue;
        }
        let mode = mode_for(&entry.path);
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
                line: entry.def.line,
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
                ],
            },
        });
    }

    candidates
}

fn detect_unused_reexports(
    reexports: &[ReExport],
    references: &ReferenceIndex,
    mode_for: impl Fn(&str) -> ProjectMode,
    shipped: impl Fn(&str) -> bool,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for reexport in reexports {
        if is_reexport_used(reexport, references) {
            continue;
        }
        let mode = mode_for(&reexport.path);
        if mode == ProjectMode::Library && exports_reexport(reexport) {
            continue;
        }
        if reexport.declared_public && shipped(&reexport.path) {
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
                line: reexport.line,
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
    let mut sites: HashMap<(&str, &str), Vec<(u32, bool)>> = HashMap::new();
    for warning in &resolution.warnings {
        let ResolveWarning::UnresolvedImport {
            import,
            file,
            line,
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
            .push((*line, guarded));
    }

    sites
        .into_iter()
        .filter_map(|((file, import), lines)| {
            unresolved_import_candidate(file, import, &lines, manifest, sources)
        })
        .collect()
}

// Anchor at a real line that can fail: line 0 is a plugin reference with no
// position, and a guarded (`TYPE_CHECKING` or optional) import never fails.
fn anchor_line(lines: &[(u32, bool)]) -> Option<u32> {
    lines
        .iter()
        .min_by_key(|(line, guarded)| (*guarded, *line == 0, *line))
        .map(|(line, _)| *line)
}

fn unresolved_import_candidate(
    file: &str,
    import: &str,
    lines: &[(u32, bool)],
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
) -> Option<IssueCandidate> {
    let line = anchor_line(lines)?;
    let guarded_only = lines.iter().all(|(_, guarded)| *guarded);
    let others = lines
        .iter()
        .map(|(other, _)| *other)
        .filter(|other| *other != line && *other != 0)
        .collect::<BTreeSet<_>>();
    let first_party = is_first_party_import(import, &sources.layout, &manifest.metadata);
    let message = if first_party {
        format!("import `{import}` in `{file}:{line}` does not resolve to a first-party module")
    } else {
        format!("import `{import}` in `{file}:{line}` could not be resolved")
    };
    let mut details = vec![
        "check for typos in first-party module names".to_owned(),
        "third-party packages may be missing from dependency declarations".to_owned(),
    ];
    if !others.is_empty() {
        let others = others
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        details.push(format!("also imported at lines {others}"));
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
        origins: std::iter::once(line)
            .chain(others.iter().copied())
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

    #[test]
    fn anchor_prefers_unguarded_then_positioned_then_earliest_line() {
        assert_eq!(anchor_line(&[]), None);
        assert_eq!(anchor_line(&[(4, true), (8, false)]), Some(8));
        assert_eq!(anchor_line(&[(0, false), (8, false)]), Some(8));
        assert_eq!(anchor_line(&[(14, false), (8, false)]), Some(8));
        assert_eq!(anchor_line(&[(0, false), (4, true)]), Some(0));
    }
}
