//! Symbol usage analysis orchestration (pipeline step 11).

use std::collections::{HashMap, HashSet};

use crate::config::{Confidence, ProjectMode};
use crate::entry::{EntryPlan, ResolvedMode};
use crate::graph::ProjectGraph;
use crate::manifest::LoadedManifest;
use crate::parser::{ParseSummary, file_module_name};
use crate::plugins::PluginHints;
use crate::reachability::ReachabilityReport;
use crate::resolver::is_first_party_import;
use crate::resolver::{ResolutionIndex, ResolveWarning};
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};
use crate::sources::DiscoveredSources;

use super::exports::{ReExport, collect_reexports, is_reexport_used};
use super::external::collect_external_symbols;
use super::graph::{
    SymbolRegistry, build_registry, collect_import_references, is_externally_referenced,
};
use super::types::SymbolReport;

/// Analyze public symbol usage and unresolved imports (§12).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn analyze_symbols(
    parse: &ParseSummary,
    resolution: &ResolutionIndex,
    reachability: &ReachabilityReport,
    entry: &EntryPlan,
    plugins: &PluginHints,
    mode: &ResolvedMode,
    graph: &ProjectGraph,
    sources: &DiscoveredSources,
    manifest: &LoadedManifest,
) -> SymbolReport {
    let reachable = reachable_file_paths(graph, reachability);
    let module_names = build_module_names(parse, sources, &reachable);
    let reachable_modules: Vec<_> = parse
        .modules
        .iter()
        .filter(|module| reachable.contains(&module.path))
        .collect();

    let registry = build_registry(&reachable_modules, &module_names);
    let references = collect_import_references(&reachable_modules, &module_names);
    let reexports = collect_reexports(&reachable_modules, &module_names, &sources.layout);
    let external_symbols =
        collect_external_symbols(&registry, entry, plugins, &module_names, &sources.layout);

    let mut candidates =
        detect_unused_exports(&registry, &references, &external_symbols, mode.mode);
    candidates.extend(detect_unused_reexports(&reexports, &references, mode.mode));
    candidates.extend(detect_unresolved_imports(
        resolution, &reachable, manifest, sources,
    ));

    candidates.sort_by(|left, right| {
        left.rule
            .as_code()
            .cmp(right.rule.as_code())
            .then_with(|| subject_key(&left.subject).cmp(&subject_key(&right.subject)))
    });

    let symbol_count = u32::try_from(registry.entries().len()).unwrap_or(u32::MAX);

    SymbolReport {
        candidates,
        symbol_count,
        external_symbols,
    }
}

fn reachable_file_paths(
    graph: &ProjectGraph,
    reachability: &ReachabilityReport,
) -> HashSet<String> {
    reachability
        .reachable
        .iter()
        .filter_map(|file_id| graph.file(*file_id).map(|node| node.path.clone()))
        .collect()
}

fn build_module_names<'a>(
    parse: &'a ParseSummary,
    sources: &DiscoveredSources,
    reachable: &HashSet<String>,
) -> HashMap<&'a str, String> {
    let mut names = HashMap::new();
    for module in &parse.modules {
        if !reachable.contains(&module.path) {
            continue;
        }
        if let Some(name) = file_module_name(&module.path, &sources.layout) {
            names.insert(module.path.as_str(), name);
        }
    }
    names
}

fn detect_unused_exports(
    registry: &SymbolRegistry,
    references: &[super::graph::SymbolReference],
    external_symbols: &indexmap::IndexSet<super::graph::SymbolId>,
    mode: ProjectMode,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for entry in registry.entries() {
        if external_symbols.contains(&entry.id) {
            continue;
        }
        if is_externally_referenced(&entry.id, references) {
            continue;
        }

        let (severity, confidence) = unused_export_severity(mode, entry.in_all);
        candidates.push(IssueCandidate {
            rule: RuleId::Yok006,
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
                    "only `from … import name` references are tracked in v0.1".to_owned(),
                    "decorated handlers, fixtures, and entry targets are excluded".to_owned(),
                ],
            },
        });
    }

    candidates
}

fn detect_unused_reexports(
    reexports: &[ReExport],
    references: &[super::graph::SymbolReference],
    mode: ProjectMode,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();

    for reexport in reexports {
        if is_reexport_used(reexport, references) {
            continue;
        }
        let (severity, confidence) = unused_reexport_severity(mode);
        candidates.push(IssueCandidate {
            rule: RuleId::Yok007,
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
    reachable: &HashSet<String>,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();
    let mut reported = HashSet::new();

    for warning in &resolution.warnings {
        let ResolveWarning::UnresolvedImport { import, file, line } = warning else {
            continue;
        };
        if !reachable.contains(file) {
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
            rule: RuleId::Yok010,
            subject: IssueSubject::Import {
                module: import.clone(),
                file: file.clone(),
                line: *line,
            },
            severity: Severity::Warning,
            confidence: Confidence::Likely,
            message,
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

fn subject_key(subject: &IssueSubject) -> String {
    match subject {
        IssueSubject::Distribution { name } | IssueSubject::Binary { name } => name.clone(),
        IssueSubject::File { path } => path.clone(),
        IssueSubject::Symbol { module, name } => format!("{module}:{name}"),
        IssueSubject::Import { module, file, line } => format!("{file}:{line}:{module}"),
    }
}

fn symbol_kind_label(kind: crate::parser::SymbolKind) -> &'static str {
    match kind {
        crate::parser::SymbolKind::Function => "function",
        crate::parser::SymbolKind::Class => "class",
        crate::parser::SymbolKind::Variable => "constant",
    }
}
