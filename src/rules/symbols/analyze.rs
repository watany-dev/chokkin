//! Symbol usage analysis orchestration (pipeline step 11).

use std::collections::{HashMap, HashSet};

use crate::config::{Confidence, ProjectMode};
use crate::entry::EntryPlan;
use crate::graph::ProjectGraph;
use crate::manifest::LoadedManifest;
use crate::parser::ParsedModule;
use crate::plugins::PluginHints;
use crate::reachability::ReachabilityReport;
use crate::resolver::is_first_party_import;
use crate::resolver::{ResolutionIndex, ResolveWarning};
use crate::rules::RuleContext;
use crate::rules::types::{
    ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity, sort_candidates,
};
use crate::sources::{DiscoveredSources, PublicSurface, path_to_module};

use super::exports::{ReExport, collect_reexports, is_reexport_used};
use super::external::collect_external_symbols;
use super::graph::{ReferenceIndex, SymbolId, SymbolRegistry, build_registry};

/// Analyze public symbol usage and unresolved imports (§12).
#[must_use]
pub fn analyze_with_context(
    context: &RuleContext<'_>,
    entry: &EntryPlan,
    plugins: &PluginHints,
    mode: ProjectMode,
    manifest: &LoadedManifest,
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
    // Root `tests/`-style packages are importable but not an API surface:
    // pytest calls their functions, so their symbols would all read as unused.
    // They still reference the symbols they import.
    let surface_modules: Vec<_> = reachable_modules
        .iter()
        .copied()
        .filter(|module| !sources.layout.in_local_package(&module.path))
        .collect();

    let registry = build_registry(&surface_modules, &module_names);
    let reference_index = ReferenceIndex::build(&reachable_modules, &module_names);
    let reexports = collect_reexports(&surface_modules, &module_names, &sources.layout);
    let external_symbols =
        collect_external_symbols(&registry, entry, plugins, &module_names, &sources.layout);

    let surface = PublicSurface::resolve(manifest.metadata.wheel_targets.as_ref(), &sources.files);
    let mut candidates = detect_unused_exports(
        &registry,
        &reference_index,
        &external_symbols,
        mode,
        surface.as_ref(),
    );
    candidates.extend(detect_unused_reexports(&reexports, &reference_index, mode));
    candidates.extend(detect_unresolved_imports(
        resolution, &reachable, manifest, sources,
    ));

    sort_candidates(&mut candidates);
    candidates
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
    registry: &SymbolRegistry,
    references: &ReferenceIndex,
    external_symbols: &indexmap::IndexSet<SymbolId>,
    mode: ProjectMode,
    surface: Option<&PublicSurface>,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for entry in registry.entries() {
        if external_symbols.contains(&entry.id) {
            continue;
        }
        if references.is_externally_referenced(&entry.id) {
            continue;
        }

        // A library symbol the wheel does not ship has no outside caller (R-05).
        let shipped = surface.is_none_or(|surface| surface.contains(&entry.path));
        let symbol_mode = if shipped { mode } else { ProjectMode::App };
        let (severity, confidence) = unused_export_severity(symbol_mode, entry.in_all);
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
    mode: ProjectMode,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for reexport in reexports {
        if is_reexport_used(reexport, references) {
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
    let mut candidates = Vec::new();
    let mut reported = HashSet::new();

    for warning in &resolution.warnings {
        let ResolveWarning::UnresolvedImport { import, file, line } = warning else {
            continue;
        };
        if !reachable.contains(file.as_str()) {
            continue;
        }
        if !reported.insert((file.clone(), *line, import.clone())) {
            continue;
        }

        let first_party = is_first_party_import(import, &sources.layout, &manifest.metadata);
        let message = if first_party {
            format!("import `{import}` in `{file}:{line}` does not resolve to a first-party module")
        } else {
            format!("import `{import}` in `{file}:{line}` could not be resolved")
        };

        candidates.push(IssueCandidate {
            rule: RuleId::Chk010,
            subject: IssueSubject::Import {
                module: import.clone(),
                file: file.clone(),
                line: *line,
                distribution: None,
            },
            severity: Severity::Warning,
            confidence: Confidence::Likely,
            message,
            workspace_member: None,
            origins: vec![Origin::Import {
                file: file.clone(),
                line: *line,
                module: import.clone(),
            }],
            explain: ExplainData {
                summary: if first_party {
                    format!("`{import}` looks like a first-party import but is unresolved")
                } else {
                    format!("`{import}` is not stdlib, first-party, or a known third-party package")
                },
                details: vec![
                    "check for typos in first-party module names".to_owned(),
                    "third-party packages may be missing from dependency declarations".to_owned(),
                ],
            },
        });
    }

    candidates
}

fn symbol_kind_label(kind: crate::parser::SymbolKind) -> &'static str {
    match kind {
        crate::parser::SymbolKind::Function => "function",
        crate::parser::SymbolKind::Class => "class",
        crate::parser::SymbolKind::Variable => "constant",
    }
}
