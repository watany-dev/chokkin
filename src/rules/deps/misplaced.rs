//! CHK005 misplaced dependency detection.

use std::collections::{HashMap, HashSet};

use crate::config::Confidence;
use crate::graph::ModuleOrigin;
use crate::manifest::LoadedManifest;
use crate::parser::{ParseSummary, SymbolKind, guarded_roots, predicate_name_module};
use crate::resolver::ResolvedImport;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};
use crate::rules::{DependencyRuleContext, RuleContext};
use crate::sources::{DiscoveredSources, FileContext, LayoutInfo, path_to_module};

use super::context::{
    DeclarationBucket, UsageContext, declaration_bucket, declaration_buckets, file_context,
    include_path_details, usage_context_for_import,
};
use super::missing::{ImportDeclarations, WorkspaceDeclaredIndex, collect_optional_imports};
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
        ..
    } = *dependency;
    let RuleContext {
        resolution,
        sources,
        reachability,
        graph,
        ..
    } = *context;
    let strengths = import_strengths(context.parse);
    let predicates = predicate_modules(context.parse, sources, reachable);
    let host_loaded = host_loaded_files(context.parse, &sources.layout, pytest11_modules);
    let eager = file_paths(graph, &reachability.eager);
    let exposed = file_paths(graph, &reachability.exposed);
    let certain = file_paths(graph, &reachability.certain);
    let mut candidates: Vec<(IssueCandidate, ImportStrength, bool)> = Vec::new();
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
        let declarations = ImportDeclarations::lookup(
            declared,
            workspace_declared,
            workspace_member,
            distribution,
        );
        if declarations.member_satisfies(usage, config) {
            continue;
        }
        let Some(declarations) = declarations.governing(strict) else {
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

        let mut strength = import
            .statement_line()
            .and_then(|line| strengths.get(&(import.file.clone(), line)))
            .copied()
            .unwrap_or(ImportStrength::TopLevel);
        // The package copes without it somewhere, so its function-local
        // imports are likely the same optional feature (#808). A platform
        // guard says it is required on that platform instead.
        let mut optional_elsewhere =
            strength == ImportStrength::Optional && (import.optional || !import.platform_guarded);
        // A file loaded only from inside functions (or never, past
        // `TYPE_CHECKING`) runs its top-level imports no earlier than a
        // function-local import would (#610).
        if !eager.contains(import.file.as_str()) {
            strength = strength.min(ImportStrength::Deferred);
        } else if !exposed.contains(import.file.as_str()) {
            // Likewise a file loaded only past an optional import may be
            // missing at runtime without breaking the package (#614).
            strength = ImportStrength::Optional;
        } else if !certain.contains(import.file.as_str()) {
            // A library's public module nothing in the package loads needs
            // it only when a caller imports that module (#802).
            strength = strength.min(ImportStrength::Exposed);
        }
        if is_opt_in_import(&import.file, &import.import_root, &sources.layout) {
            strength = strength.min(ImportStrength::OptIn);
        }
        // Test support the package loads itself is runtime code all the same.
        if strength < ImportStrength::TopLevel && is_test_support(&import.file, &sources.layout) {
            strength = strength.min(ImportStrength::TestSupport);
        }
        if strength == ImportStrength::Deferred {
            let member = sources
                .layout
                .member_for(&import.file)
                .map(|(member, _)| member.path.as_str());
            optional_elsewhere |= predicates.contains(&(member, import.import_root.as_str()));
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
            if let Some((candidate, current, optional)) = candidates.get_mut(index) {
                *optional |= optional_elsewhere;
                if strength > *current {
                    *current = strength;
                    candidate.origins = vec![import_origin(import)];
                }
            }
            continue;
        }
        reported.insert(report_key, candidates.len());

        let mut contexts: Vec<String> = declarations
            .iter()
            .map(|dep| declaration_bucket(&dep.context, &config.dependencies).label())
            .collect();
        contexts.sort();
        contexts.dedup();

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
            optional_elsewhere,
        ));
    }

    candidates
        .into_iter()
        .map(|(candidate, strength, optional_elsewhere)| {
            if strength == ImportStrength::Deferred && optional_elsewhere {
                ImportStrength::DeferredOptional.apply(candidate)
            } else {
                strength.apply(candidate)
            }
        })
        .collect()
}

/// How firmly the runtime code needs a dev-only distribution. `--fix` moves it
/// to runtime only for `TopLevel` (Certain); optional and function-local
/// imports work without it until the code path runs (#583).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ImportStrength {
    /// In a module the distribution itself loads (#731).
    HostLoaded,
    /// In airflow's `example_dags/` or a subpackage named after the import
    /// (`mlflow/pyspark/`, `mlflow/langchain/` for `langchain_community`).
    OptIn,
    /// In test support the package ships (`django.test`,
    /// `django/contrib/admin/tests.py`): only a test suite loads it (#808).
    TestSupport,
    /// `try`/`except ImportError`, `suppress(ImportError)` or
    /// platform-guarded, or in a file loaded only past such an import.
    Optional,
    /// [`Self::Deferred`], and runtime code elsewhere imports it under a
    /// guard or names it in an `is_x…` predicate (#808). Assigned only after
    /// every import has been weighed.
    DeferredOptional,
    /// Only inside a function body, or in a file runtime code loads only
    /// from one or from a `TYPE_CHECKING` block.
    Deferred,
    /// At top level of a library's public module that neither the package's
    /// own `__init__` nor an entry point loads, only callers and tests (#802).
    Exposed,
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
            Self::OptIn => {
                candidate.severity = Severity::Info;
                "imported only by example DAGs or by a subpackage named after it, which load only when opted into"
            },
            Self::TestSupport => {
                candidate.severity = Severity::Info;
                "imported only by test support the package ships, which only a test suite loads"
            },
            Self::DeferredOptional => {
                candidate.severity = Severity::Info;
                "imported only inside functions, and runtime code elsewhere guards it as optional or checks its types by name"
            },
            Self::Optional => {
                candidate.severity = Severity::Info;
                "imported only under try/except ImportError, a platform guard or a check that it is already imported, or by modules loaded only that way"
            },
            Self::Deferred => {
                "imported only inside functions or by modules runtime code loads only from inside functions or TYPE_CHECKING blocks"
            },
            Self::Exposed => {
                "imported only by public modules that the package itself does not load: callers need it only when they import them"
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

/// Whether `file` sits in a subpackage named `example_dags` (airflow loads
/// them only with `load_examples`) or after the import root, or one the root
/// extends with `_` (`langchain` for `langchain_community`): an opt-in part
/// of the package, not a requirement of it (#781). The top-level package
/// does not count: `dagster_shared` imported from `dagster` is the package's
/// own requirement. Example DAGs ship in the wheel, so they stay runtime
/// files for the other rules.
fn is_opt_in_import(file: &str, import_root: &str, layout: &LayoutInfo) -> bool {
    let Some(module) = path_to_module(file, layout) else {
        return false;
    };
    let package = if file.ends_with("__init__.py") {
        module.as_str()
    } else {
        module.rsplit_once('.').map_or("", |(package, _)| package)
    };
    package.split('.').skip(1).any(|name| {
        name == "example_dags"
            || import_root
                .strip_prefix(name)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
    })
}

/// Whether `file` is test support the package ships: a module or subpackage
/// below the top-level package named `test`, `tests` or `testing`
/// (`django/test/selenium.py`, `django/contrib/admin/tests.py`). They stay
/// runtime files for the other rules: callers import them from their own
/// tests (§10).
fn is_test_support(file: &str, layout: &LayoutInfo) -> bool {
    let Some(module) = path_to_module(file, layout) else {
        return false;
    };
    let mut names: Vec<&str> = module.split('.').skip(1).collect();
    // A `test.py` module is as likely a command (dbt's `task/test.py`).
    if !file.ends_with("__init__.py") && names.last() == Some(&"test") {
        names.pop();
    }
    names
        .iter()
        .any(|name| matches!(*name, "test" | "tests" | "testing"))
}

/// Import roots the project's own `is_x…` functions name (streamlit's
/// `is_polars_dataframe`), each with the workspace member path defining it:
/// the member checks objects of `x` without importing it, so treats `x` as
/// optional (#808). Another member's predicate says nothing about this one,
/// and neither does one only tests or unreachable code define.
fn predicate_modules<'a>(
    parse: &'a ParseSummary,
    sources: &'a DiscoveredSources,
    reachable: &HashSet<&str>,
) -> HashSet<(Option<&'a str>, &'a str)> {
    parse
        .modules
        .iter()
        .filter(|module| {
            reachable.contains(module.path.as_str())
                && file_context(&module.path, sources) == FileContext::Runtime
        })
        .flat_map(|module| {
            let member = sources
                .layout
                .member_for(&module.path)
                .map(|(member, _)| member.path.as_str());
            module
                .symbols
                .iter()
                .filter(|symbol| symbol.kind == SymbolKind::Function)
                .filter_map(|symbol| predicate_name_module(&symbol.name))
                .flat_map(guarded_roots)
                .map(move |root| (member, root))
        })
        .collect()
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

    #[test]
    fn opt_in_imports_sit_under_example_dags_or_a_subpackage_named_after_them() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Flat,
            packages: vec![
                "airflow".to_owned(),
                "dagster".to_owned(),
                "mlflow".to_owned(),
            ],
            ..LayoutInfo::default()
        };
        let opt_in = |file: &str, root: &str| is_opt_in_import(file, root, &layout);
        assert!(opt_in("airflow/example_dags/tutorial.py", "duckdb"));
        assert!(opt_in("mlflow/pyspark/ml/_autolog.py", "pyspark"));
        assert!(opt_in(
            "mlflow/langchain/__init__.py",
            "langchain_community"
        ));
        assert!(!opt_in("mlflow/pytorch/_lightning_autolog.py", "torch"));
        assert!(!opt_in("mlflow/langchainx/run.py", "langchain"));
        assert!(!opt_in("mlflow/pyspark.py", "pyspark"));
        assert!(!opt_in("dagster/core/run.py", "dagster_shared"));
        assert!(!opt_in("/abs/plugin.py", "_pytest"));
    }

    /// #808: a `test`/`tests`/`testing` subpackage or module is test support,
    /// a top-level package of that name is not.
    #[test]
    fn test_support_sits_below_the_top_level_package() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Flat,
            packages: vec!["django".to_owned(), "testing".to_owned()],
            ..LayoutInfo::default()
        };
        let support = |file: &str| is_test_support(file, &layout);
        assert!(support("django/test/selenium.py"));
        assert!(support("django/contrib/admin/tests.py"));
        assert!(support("django/testing/__init__.py"));
        assert!(support("django/test/__init__.py"));
        // dbt's `test` command.
        assert!(!support("django/task/test.py"));
        assert!(!support("django/contrib/testimonials.py"));
        assert!(!support("testing/runner.py"));
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
