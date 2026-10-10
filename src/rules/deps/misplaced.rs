//! CHK005 misplaced dependency detection.

use std::collections::{HashMap, HashSet};

use crate::config::Confidence;
use crate::graph::ModuleOrigin;
use crate::manifest::LoadedManifest;
use crate::parser::{ParseSummary, SymbolKind};
use crate::resolver::ResolvedImport;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};
use crate::rules::{DependencyRuleContext, RuleContext};
use crate::sources::{LayoutInfo, path_to_module};

use super::context::{
    DeclarationBucket, UsageContext, declaration_bucket, declaration_buckets, include_path_details,
    is_directly_declared, usage_context_for_import,
};
use super::missing::{
    WorkspaceDeclaredIndex, collect_optional_imports, governing_declarations, member_declarations,
};
use super::used::{DeclaredIndex, file_paths};

/// Detect runtime usage of dev-only dependencies (and similar mismatches).
#[allow(clippy::too_many_lines)]
pub(super) fn detect_misplaced_dependencies(
    declared: &DeclaredIndex<'_>,
    dependency: &DependencyRuleContext<'_>,
    reachable: &HashSet<&str>,
    workspace_declared: &[WorkspaceDeclaredIndex<'_>],
    pytest11_modules: &HashSet<&str>,
) -> Vec<IssueCandidate> {
    let DependencyRuleContext {
        rules: context,
        config,
        strict,
    } = *dependency;
    let RuleContext {
        resolution,
        sources,
        reachability,
        graph,
        ..
    } = *context;
    let strengths = import_strengths(context.parse);
    let host_loaded = host_loaded_files(context.parse, &sources.layout, pytest11_modules);
    let eager = file_paths(graph, &reachability.eager);
    let certain = file_paths(graph, &reachability.certain);
    let mut candidates: Vec<(IssueCandidate, ImportStrength)> = Vec::new();
    let mut reported: HashMap<(String, String), usize> = HashMap::new();

    for import in &resolution.imports {
        if import.origin != ModuleOrigin::ThirdParty {
            continue;
        }
        let Some(distribution) = import.distribution.as_ref() else {
            continue;
        };
        if !reachable.contains(import.file.as_str()) {
            continue;
        }

        let usage = usage_context_for_import(&import.file, import.context, sources);
        if usage != UsageContext::Runtime {
            continue;
        }

        let workspace_member = import.workspace_member.as_deref();
        // Same as CHK003/CHK004: a member that declares it for runtime is
        // satisfied, even when the root only lists it in a dev group.
        if workspace_member
            .and_then(|member_id| member_declarations(workspace_declared, member_id, distribution))
            .is_some_and(|deps| is_directly_declared(deps, usage, config))
        {
            continue;
        }
        let declarations = governing_declarations(
            declared,
            workspace_declared,
            workspace_member,
            distribution,
            strict,
        );
        let Some(declarations) = declarations else {
            continue;
        };

        let buckets: Vec<DeclarationBucket> = declarations
            .iter()
            .flat_map(|&dep| declaration_buckets(dep, &config.dependencies))
            .collect();
        let has_runtime = buckets.iter().any(|bucket| {
            matches!(
                bucket,
                DeclarationBucket::Runtime | DeclarationBucket::Optional(_)
            )
        });
        if has_runtime {
            continue;
        }

        let has_dev_only = buckets
            .iter()
            .any(|bucket| matches!(bucket, DeclarationBucket::Dev | DeclarationBucket::Type));
        if !has_dev_only {
            continue;
        }

        let mut strength = strengths
            .get(&(import.file.clone(), import.line))
            .copied()
            .unwrap_or(ImportStrength::TopLevel);
        // A file loaded only from inside functions (or never, past
        // `TYPE_CHECKING`) runs its top-level imports no earlier than a
        // function-local import would (#610).
        if !eager.contains(import.file.as_str()) {
            strength = strength.min(ImportStrength::Deferred);
        } else if !certain.contains(import.file.as_str()) {
            // Likewise a file loaded only past an optional import may be
            // missing at runtime without breaking the package (#614).
            strength = ImportStrength::Optional;
        }
        if host_loaded.contains(&(import.file.as_str(), distribution.as_str())) {
            strength = ImportStrength::HostLoaded;
        }
        let report_key = (
            workspace_member.unwrap_or_default().to_owned(),
            distribution.clone(),
        );
        // Every import of the distribution counts: one top-level import is
        // enough for Certain, whichever import came first (#583).
        if let Some(&index) = reported.get(&report_key) {
            if let Some((candidate, current)) = candidates.get_mut(index)
                && strength > *current
            {
                *current = strength;
                candidate.origins = vec![import_origin(import)];
            }
            continue;
        }
        reported.insert(report_key, candidates.len());

        let contexts: Vec<String> = declarations
            .iter()
            .map(|dep| declaration_bucket(&dep.context, &config.dependencies).label())
            .collect();

        candidates.push((
            IssueCandidate {
                rule: RuleId::Chk005,
                subject: IssueSubject::Distribution {
                    name: distribution.clone(),
                },
                severity: Severity::Warning,
                confidence: Confidence::Certain,
                message: misplaced_message(distribution, workspace_member, &contexts),
                workspace_member: workspace_member.map(str::to_owned),
                origins: vec![import_origin(import)],
                explain: ExplainData {
                    summary: format!("{distribution} is misplaced for runtime usage"),
                    details: std::iter::once(format!("declared in: {}", contexts.join(", ")))
                        .chain(
                            declarations
                                .iter()
                                .flat_map(|&dep| include_path_details(dep)),
                        )
                        .collect(),
                },
            },
            strength,
        ));
    }

    candidates
        .into_iter()
        .map(|(candidate, strength)| strength.apply(candidate))
        .collect()
}

/// How firmly the runtime code needs a dev-only distribution. `--fix` moves it
/// to runtime only for `TopLevel` (Certain); optional and function-local
/// imports work without it until the code path runs (#583).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ImportStrength {
    /// In a module the distribution itself loads (#731).
    HostLoaded,
    /// `try`/`except ImportError`, `suppress(ImportError)` or
    /// platform-guarded, or in a file loaded only past such an import.
    Optional,
    /// Only inside a function body, or in a file runtime code loads only
    /// from one or from a `TYPE_CHECKING` block.
    Deferred,
    TopLevel,
}

impl ImportStrength {
    fn apply(self, mut candidate: IssueCandidate) -> IssueCandidate {
        let detail = match self {
            Self::TopLevel => return candidate,
            Self::HostLoaded => {
                candidate.severity = Severity::Info;
                "imported only by modules it loads itself: a pytest plugin or an IPython extension"
            },
            Self::Optional => {
                candidate.severity = Severity::Info;
                "imported only under try/except ImportError, a platform guard or a check that it is already imported, or by modules loaded only that way"
            },
            Self::Deferred => {
                "imported only inside functions or by modules runtime code loads only from inside functions or TYPE_CHECKING blocks"
            },
        };
        candidate.confidence = Confidence::Likely;
        candidate.explain.details.push(detail.to_owned());
        candidate
    }
}

fn import_strengths(parse: &ParseSummary) -> HashMap<(String, u32), ImportStrength> {
    let optional = collect_optional_imports(parse);
    let mut strengths = HashMap::new();
    for module in &parse.modules {
        let sites = module
            .imports
            .iter()
            .map(|import| (import.line, import.deferred))
            .chain(
                module
                    .dynamic_imports
                    .iter()
                    .map(|dynamic| (dynamic.line, dynamic.deferred)),
            );
        for (line, deferred) in sites {
            let key = (module.path.clone(), line);
            let strength = if optional.contains(&key) {
                ImportStrength::Optional
            } else if deferred {
                ImportStrength::Deferred
            } else {
                ImportStrength::TopLevel
            };
            strengths.insert(key, strength);
        }
    }
    strengths
}

pub(super) fn pytest11_modules<'a>(
    manifests: impl Iterator<Item = &'a LoadedManifest>,
) -> HashSet<&'a str> {
    manifests
        .flat_map(|manifest| &manifest.entry_points)
        .filter(|entry| entry.group == "pytest11")
        .filter_map(|entry| entry.target.split(':').next())
        .map(str::trim)
        .collect()
}

/// `(file, distribution)` pairs where the distribution's program loads the
/// file, so the file's import of it only runs with it installed (#731):
/// pytest loads a `pytest11` entry-point target or a module defining a
/// top-level `pytest_*` hook, `IPython` one defining `load_ipython_extension`.
fn host_loaded_files<'a>(
    parse: &'a ParseSummary,
    layout: &LayoutInfo,
    pytest11_modules: &HashSet<&str>,
) -> HashSet<(&'a str, &'static str)> {
    let mut loaded = HashSet::new();
    for module in &parse.modules {
        let defines = |is_hook: fn(&str) -> bool| {
            module
                .symbols
                .iter()
                .any(|symbol| symbol.kind == SymbolKind::Function && is_hook(&symbol.name))
        };
        let registered = path_to_module(&module.path, layout)
            .is_some_and(|name| pytest11_modules.contains(name.as_str()));
        if registered || defines(|name| name.starts_with("pytest_")) {
            loaded.insert((module.path.as_str(), "pytest"));
        }
        if defines(|name| name == "load_ipython_extension") {
            loaded.insert((module.path.as_str(), "ipython"));
        }
    }
    loaded
}

fn import_origin(import: &ResolvedImport) -> Origin {
    Origin::Import {
        file: import.file.clone(),
        line: import.line,
        module: import.full_module.clone(),
    }
}

fn misplaced_message(
    distribution: &str,
    workspace_member: Option<&str>,
    contexts: &[String],
) -> String {
    if let Some(member_id) = workspace_member {
        return format!(
            "{distribution} is used from runtime code in workspace member {member_id} but only declared in {}",
            contexts.join(", ")
        );
    }

    format!(
        "{distribution} is used from runtime code but only declared in {}",
        contexts.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParsedModule, SymbolDef};
    use crate::sources::ProjectLayout;

    fn module(path: &str, symbols: &[(&str, SymbolKind)]) -> ParsedModule {
        ParsedModule {
            path: path.to_owned(),
            symbols: symbols
                .iter()
                .map(|&(name, kind)| SymbolDef {
                    name: name.to_owned(),
                    kind,
                    line: 1,
                    is_public: true,
                    decorators: Vec::new(),
                    in_type_checking: false,
                    used_in_module: false,
                })
                .collect(),
            ..ParsedModule::default()
        }
    }

    /// #731: a file is host-loaded only for the host that loads it.
    #[test]
    fn host_loaded_files_pair_each_file_with_its_own_host() {
        let pytest11 = HashSet::from(["acme.plugin", "acme.objects"]);
        let parse = ParseSummary {
            modules: vec![
                module("src/acme/plugin.py", &[]),
                module("src/acme/objects/__init__.py", &[]),
                module("src/acme/cli.py", &[("main", SymbolKind::Function)]),
                module(
                    "src/acme/hooks.py",
                    &[("pytest_configure", SymbolKind::Function)],
                ),
                module(
                    "src/acme/ext.py",
                    &[("load_ipython_extension", SymbolKind::Function)],
                ),
                module(
                    "src/acme/lookalikes.py",
                    &[
                        ("pytest_plugins", SymbolKind::Variable),
                        ("pytest_Helper", SymbolKind::Class),
                        ("load_ipython_extension", SymbolKind::Variable),
                        ("run_pytest_configure", SymbolKind::Function),
                    ],
                ),
            ],
        };
        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            package_root: "src".to_owned(),
            packages: vec!["acme".to_owned()],
            ..LayoutInfo::default()
        };
        let mut loaded: Vec<_> = host_loaded_files(&parse, &layout, &pytest11)
            .into_iter()
            .collect();
        loaded.sort_unstable();
        assert_eq!(
            loaded,
            [
                ("src/acme/ext.py", "ipython"),
                ("src/acme/hooks.py", "pytest"),
                ("src/acme/objects/__init__.py", "pytest"),
                ("src/acme/plugin.py", "pytest"),
            ]
        );
    }
}
