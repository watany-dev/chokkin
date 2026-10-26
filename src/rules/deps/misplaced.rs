//! CHK005 misplaced dependency detection.

use std::collections::{HashMap, HashSet};

use crate::config::Confidence;
use crate::graph::ModuleOrigin;
use crate::manifest::LoadedManifest;
use crate::parser::{ParseSummary, SymbolKind, guarded_roots, predicate_name_module};
use crate::resolver::{ResolutionIndex, ResolvedImport};
use crate::rules::ignore::IgnoreMatcher;
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
#[expect(
    clippy::too_many_lines,
    reason = "one pass over the imports that weighs every signal for a finding together"
)]
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
    let integrations = integration_packages(declared, dependency, workspace_declared, reachable);
    let eager = file_paths(graph, &reachability.eager);
    let exposed = file_paths(graph, &reachability.exposed);
    let certain = file_paths(graph, &reachability.certain);
    let matcher = IgnoreMatcher::build(config, context.parse, &ResolutionIndex::default());
    // The bool: every site so far is ignored inline, file-level or vendored.
    let mut candidates: Vec<(IssueCandidate, Weight, bool)> = Vec::new();
    let mut reported: HashMap<(String, String), usize> = HashMap::new();

    for import in &resolution.imports {
        if import.origin != ModuleOrigin::ThirdParty || import.never_runs {
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

        let member = sources
            .layout
            .member_for(&import.file)
            .map(|(member, _)| member.path.as_str());
        let weight = ImportSignals {
            site: import
                .statement_line()
                .and_then(|line| strengths.get(&(import.file.clone(), line)))
                .copied()
                .unwrap_or(ImportStrength::TopLevel),
            platform_guard_only: import.platform_guarded && !import.optional,
            eager: eager.contains(import.file.as_str()),
            exposed: exposed.contains(import.file.as_str()),
            certain: certain.contains(import.file.as_str()),
            opt_in: is_opt_in_import(&import.file, &import.import_root, &sources.layout)
                || in_integration(&import.file, &sources.layout, &integrations),
            command: is_management_command(&import.file),
            test_support: is_test_support(&import.file, &sources.layout),
            predicate: predicates.contains(&(member, import.import_root.as_str())),
            host_loaded: host_loaded.contains(&(import.file.as_str(), distribution.as_str())),
        }
        .weigh();
        let report_key = (
            workspace_member.unwrap_or_default().to_owned(),
            distribution.clone(),
        );
        // An ignored site drops out before the sites are weighed, so it neither
        // hides the others nor lends them its strength; only when every site
        // is ignored does the finding stand on them, to be suppressed (#1107).
        let ignored = matcher.ignores_site(RuleId::Chk005, &import.file, import.line);
        if let Some(&index) = reported.get(&report_key) {
            if let Some((candidate, current, all_ignored)) = candidates.get_mut(index) {
                let replace = match (ignored, *all_ignored) {
                    (true, false) => false,
                    (false, true) => {
                        *current = weight;
                        *all_ignored = false;
                        true
                    },
                    _ => current.absorb(weight),
                };
                if replace {
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
            weight,
            ignored,
        ));
    }

    candidates
        .into_iter()
        .map(|(candidate, weight, _)| weight.settle().apply(candidate))
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
    /// In a Django management command, which runs only when invoked by name
    /// (#1242).
    Command,
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
    /// Severity and confidence of a finding this strong (§10). Only
    /// `TopLevel` is Certain, so only it reaches `--fix`'s `MoveToRuntime`.
    fn verdict(self) -> (Severity, Confidence) {
        match self {
            Self::TopLevel => (Severity::Warning, Confidence::Certain),
            Self::Deferred | Self::Exposed => (Severity::Warning, Confidence::Likely),
            Self::HostLoaded
            | Self::OptIn
            | Self::Command
            | Self::TestSupport
            | Self::Optional
            | Self::DeferredOptional => (Severity::Info, Confidence::Likely),
        }
    }

    fn detail(self) -> Option<&'static str> {
        Some(match self {
            Self::TopLevel => return None,
            Self::HostLoaded => {
                "imported only by modules it loads itself: a pytest plugin or an IPython extension"
            },
            Self::OptIn => {
                "imported only by example DAGs or by a subpackage named after it or after a library it integrates, which load only when opted into"
            },
            Self::Command => {
                "imported only by Django management commands, which load only when that command runs"
            },
            Self::TestSupport => {
                "imported only by test support the package ships, which only a test suite loads"
            },
            Self::DeferredOptional => {
                "imported only inside functions, and runtime code elsewhere guards it as optional or checks its types by name"
            },
            Self::Optional => {
                "imported only under try/except ImportError, a platform guard or a check that it is already imported, or by modules loaded only that way"
            },
            Self::Deferred => {
                "imported only inside functions or by modules runtime code loads only from inside functions or TYPE_CHECKING blocks"
            },
            Self::Exposed => {
                "imported only by public modules that the package itself does not load: callers need it only when they import them"
            },
        })
    }

    fn apply(self, mut candidate: IssueCandidate) -> IssueCandidate {
        (candidate.severity, candidate.confidence) = self.verdict();
        if let Some(detail) = self.detail() {
            candidate.explain.details.push(detail.to_owned());
        }
        candidate
    }
}

/// What CHK005 knows about one runtime import of a dev-only distribution:
/// the import site, and where the file importing it sits.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent signals about one import site, weighed together by CHK005"
)]
#[derive(Debug, Clone, Copy)]
struct ImportSignals {
    /// From the site alone: `Optional`, `Deferred` or `TopLevel`.
    site: ImportStrength,
    /// Guarded only by a platform check, which says the import is required
    /// on that platform rather than optional.
    platform_guard_only: bool,
    /// The file is in `ReachabilityReport::eager` / `exposed` / `certain`.
    eager: bool,
    exposed: bool,
    certain: bool,
    opt_in: bool,
    command: bool,
    test_support: bool,
    /// The file's workspace member defines an `is_x…` predicate naming the
    /// import root (#808).
    predicate: bool,
    host_loaded: bool,
}

/// How firmly one import, or every import of a distribution so far, needs
/// it, and whether any of them shows the member treats it as optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Weight {
    strength: ImportStrength,
    optional_elsewhere: bool,
}

impl ImportSignals {
    fn weigh(self) -> Weight {
        let mut strength = self.site;
        // The package copes without it somewhere, so its function-local
        // imports are likely the same optional feature (#808). A platform
        // guard says it is required on that platform instead.
        let mut optional_elsewhere =
            strength == ImportStrength::Optional && !self.platform_guard_only;
        if !self.eager {
            // A file loaded only from inside functions (or never, past
            // `TYPE_CHECKING`) runs its top-level imports no earlier than a
            // function-local import would (#610).
            strength = strength.min(ImportStrength::Deferred);
        } else if !self.exposed {
            // Likewise a file loaded only past an optional import may be
            // missing at runtime without breaking the package (#614).
            strength = strength.min(ImportStrength::Optional);
        } else if !self.certain {
            // A library's public module nothing in the package loads needs
            // it only when a caller imports that module (#802).
            strength = strength.min(ImportStrength::Exposed);
        }
        if self.opt_in {
            strength = strength.min(ImportStrength::OptIn);
        }
        if self.command {
            strength = strength.min(ImportStrength::Command);
        }
        // Test support the package loads itself is runtime code all the same.
        if strength < ImportStrength::TopLevel && self.test_support {
            strength = strength.min(ImportStrength::TestSupport);
        }
        if strength == ImportStrength::Deferred {
            optional_elsewhere |= self.predicate;
        }
        if self.host_loaded {
            strength = ImportStrength::HostLoaded;
        }
        Weight {
            strength,
            optional_elsewhere,
        }
    }
}

impl Weight {
    /// Fold in another import of the same distribution. Every import counts:
    /// one top-level import is enough for Certain, whichever came first
    /// (#583). Returns whether `other` is now the strongest, so its origin is
    /// the one to report.
    fn absorb(&mut self, other: Self) -> bool {
        self.optional_elsewhere |= other.optional_elsewhere;
        let stronger = other.strength > self.strength;
        if stronger {
            self.strength = other.strength;
        }
        stronger
    }

    fn settle(self) -> ImportStrength {
        if self.strength == ImportStrength::Deferred && self.optional_elsewhere {
            ImportStrength::DeferredOptional
        } else {
            self.strength
        }
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
    package_of(file, &module).split('.').skip(1).any(|name| {
        name == "example_dags"
            || import_root
                .strip_prefix(name)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
    })
}

/// Subpackages (`mlflow.genai.scorers.ragas`) named after a third-party
/// library one of their runtime files imports, when the project declares
/// that library only as dev or an extra: an integration with an opt-in library,
/// so everything it imports is opt-in as well (#1242). The top-level package
/// does not count, as in [`is_opt_in_import`], and neither does a subpackage
/// holding all of its workspace member's runtime code: that distribution
/// (the llama-index snowflake reader) is the integration, and installing it
/// is the opt-in.
fn integration_packages(
    declared: &DeclaredIndex<'_>,
    dependency: &DependencyRuleContext<'_>,
    workspace_declared: &[WorkspaceDeclaredIndex<'_>],
    reachable: &HashSet<&str>,
) -> HashSet<String> {
    let sources = dependency.rules.sources;
    let member_of = |file: &str| {
        sources
            .layout
            .member_for(file)
            .map(|(member, _)| member.path.as_str())
    };
    let mut packages: HashMap<String, Option<&str>> = HashMap::new();
    for import in &dependency.rules.resolution.imports {
        if import.origin != ModuleOrigin::ThirdParty
            || import.never_runs
            || !reachable.contains(import.file.as_str())
            || usage_context_for_import(&import.file, import.context, sources)
                != UsageContext::Runtime
        {
            continue;
        }
        let Some(module) = path_to_module(&import.file, &sources.layout) else {
            continue;
        };
        let names: Vec<&str> = package_of(&import.file, &module).split('.').collect();
        let Some(depth) = names
            .iter()
            .skip(1)
            .position(|name| *name == import.import_root)
        else {
            continue;
        };
        let Some(distribution) = import.distribution.as_ref() else {
            continue;
        };
        let declarations = ImportDeclarations::lookup(
            declared,
            workspace_declared,
            import.workspace_member.as_deref(),
            distribution,
        );
        // A member requiring it itself makes the subpackage its own code, and
        // an undeclared library is missing (CHK003), not opted into.
        let opt_in = !declarations.member_satisfies(UsageContext::Runtime, dependency.config)
            && declarations
                .governing(dependency.strict)
                .is_some_and(|declarations| {
                    declarations.iter().all(|&dep| {
                        !declaration_buckets(dep, &dependency.config.dependencies)
                            .contains(&DeclarationBucket::Runtime)
                    })
                });
        if opt_in {
            packages.insert(
                names.get(..depth + 2).unwrap_or_default().join("."),
                member_of(&import.file),
            );
        }
    }
    packages.retain(|package, member| {
        sources.files.iter().any(|file| {
            file.context == FileContext::Runtime
                && member_of(&file.path) == *member
                && path_to_module(&file.path, &sources.layout)
                    .is_some_and(|module| !under_package(package_of(&file.path, &module), package))
        })
    });
    packages.into_keys().collect()
}

/// Whether `package` is `ancestor` or below it.
fn under_package(package: &str, ancestor: &str) -> bool {
    package
        .strip_prefix(ancestor)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// Whether `file` sits in one of the [`integration_packages`].
fn in_integration(file: &str, layout: &LayoutInfo, integrations: &HashSet<String>) -> bool {
    if integrations.is_empty() {
        return false;
    }
    let Some(module) = path_to_module(file, layout) else {
        return false;
    };
    let package = package_of(file, &module);
    integrations
        .iter()
        .any(|integration| under_package(package, integration))
}

/// The package `file` (module `module`) belongs to: itself for an
/// `__init__.py`.
fn package_of<'a>(file: &str, module: &'a str) -> &'a str {
    if file.ends_with("__init__.py") {
        module
    } else {
        module.rsplit_once('.').map_or("", |(package, _)| package)
    }
}

/// Whether `file` is in a Django app's `management/commands/`: Django imports
/// a command module only to run that command, never at startup, so its
/// top-level imports run no earlier than a function-local one (#1242).
fn is_management_command(file: &str) -> bool {
    file.starts_with("management/commands/") || file.contains("/management/commands/")
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

/// kani proofs of the §10 CHK005 table over every pair of imports of one
/// distribution, run against `weigh` / `absorb` / `settle` (`make kani`).
#[cfg(kani)]
mod verification {
    use super::*;

    /// A member-wide fact, so both imports share `predicate`. Reachability
    /// nests `certain ⊆ exposed ⊆ eager` (`Reachability.tla`).
    fn any_signals(predicate: bool) -> ImportSignals {
        let certain = kani::any();
        let exposed = certain || kani::any();
        let eager = exposed || kani::any();
        ImportSignals {
            site: match kani::any::<u8>() % 3 {
                0 => ImportStrength::Optional,
                1 => ImportStrength::Deferred,
                _ => ImportStrength::TopLevel,
            },
            platform_guard_only: kani::any(),
            eager,
            exposed,
            certain,
            opt_in: kani::any(),
            command: kani::any(),
            test_support: kani::any(),
            predicate,
            host_loaded: kani::any(),
        }
    }

    /// One import's row of the table, read off §10 sentence by sentence.
    fn spec_strength(signals: ImportSignals) -> ImportStrength {
        use ImportStrength as S;
        if signals.host_loaded {
            return S::HostLoaded;
        }
        if signals.opt_in {
            return S::OptIn;
        }
        if signals.command {
            return S::Command;
        }
        let position = match (
            signals.site,
            signals.eager,
            signals.exposed,
            signals.certain,
        ) {
            (S::Optional, ..) => S::Optional,
            (_, false, ..) => S::Deferred,
            (_, true, false, _) => S::Optional,
            (S::TopLevel, true, true, false) => S::Exposed,
            (site, ..) => site,
        };
        if signals.test_support && position != S::TopLevel {
            S::TestSupport
        } else {
            position
        }
    }

    /// The strongest import decides, except that only function-local
    /// imports plus optional evidence in the member are Info (#808).
    fn spec_settle(a: ImportSignals, b: ImportSignals) -> ImportStrength {
        let strongest = spec_strength(a).max(spec_strength(b));
        let optional =
            |s: ImportSignals| s.site == ImportStrength::Optional && !s.platform_guard_only;
        if strongest == ImportStrength::Deferred && (optional(a) || optional(b) || a.predicate) {
            ImportStrength::DeferredOptional
        } else {
            strongest
        }
    }

    #[kani::proof]
    fn chk005_strength_follows_spec_table() {
        let predicate = kani::any();
        let (a, b) = (any_signals(predicate), any_signals(predicate));
        let mut weight = a.weigh();
        let b_is_origin = weight.absorb(b.weigh());
        let strength = weight.settle();
        assert_eq!(strength, spec_settle(a, b));

        // Order does not matter: the first import found decides nothing (#583).
        let mut reversed = b.weigh();
        reversed.absorb(a.weigh());
        assert_eq!(reversed.settle(), strength);

        // The reported origin is an import as strong as the finding.
        let origin = if b_is_origin { b } else { a };
        assert_eq!(origin.weigh().strength, weight.strength);

        // Certain, and so `--fix`'s MoveToRuntime, needs a top-level import
        // in a file the package loads itself, outside opt-in, command and
        // host-loaded modules.
        let certain_site = |s: ImportSignals| {
            s.site == ImportStrength::TopLevel
                && s.certain
                && !s.opt_in
                && !s.command
                && !s.host_loaded
        };
        let (_, confidence) = strength.verdict();
        assert_eq!(
            confidence == Confidence::Certain,
            certain_site(a) || certain_site(b)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParsedModule, SymbolDef};

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

    fn import(edit: impl FnOnce(&mut ImportSignals)) -> ImportSignals {
        let mut signals = ImportSignals {
            site: ImportStrength::TopLevel,
            platform_guard_only: false,
            eager: true,
            exposed: true,
            certain: true,
            opt_in: false,
            command: false,
            test_support: false,
            predicate: false,
            host_loaded: false,
        };
        edit(&mut signals);
        signals
    }

    fn settle(imports: &[ImportSignals]) -> ImportStrength {
        let mut weights = imports.iter().map(|signals| signals.weigh());
        let mut weight = weights.next().expect("at least one import");
        for other in weights {
            weight.absorb(other);
        }
        weight.settle()
    }

    /// `--explain` says why a finding below Certain is not Certain, so
    /// every other strength names its own reason.
    #[test]
    fn each_strength_below_top_level_explains_itself() {
        use ImportStrength as S;
        assert_eq!(S::TopLevel.detail(), None);
        let details: std::collections::BTreeSet<_> = [
            S::HostLoaded,
            S::OptIn,
            S::Command,
            S::TestSupport,
            S::DeferredOptional,
            S::Optional,
            S::Deferred,
            S::Exposed,
        ]
        .map(|strength| strength.detail().expect("a detail"))
        .into();
        assert_eq!(details.len(), 8);
        assert!(
            details
                .iter()
                .all(|detail| detail.starts_with("imported only"))
        );
    }

    /// The §10 CHK005 table, one row per sentence.
    #[test]
    fn import_strength_follows_the_spec_table() {
        use ImportStrength as S;
        let top = import(|_| ());
        let deferred = import(|s| s.site = S::Deferred);
        let optional = import(|s| s.site = S::Optional);
        let platform = import(|s| (s.site, s.platform_guard_only) = (S::Optional, true));
        let support = |certain| import(|s| (s.test_support, s.certain) = (true, certain));
        let cases = [
            ("top level", vec![top], S::TopLevel),
            (
                "one top level is enough (#583)",
                vec![deferred, top, optional],
                S::TopLevel,
            ),
            ("all deferred", vec![deferred, deferred], S::Deferred),
            ("all optional", vec![optional, platform], S::Optional),
            (
                "not eager (#610)",
                vec![import(|s| {
                    (s.eager, s.exposed, s.certain) = (false, false, false);
                })],
                S::Deferred,
            ),
            (
                "eager, not exposed (#614)",
                vec![import(|s| (s.exposed, s.certain) = (false, false))],
                S::Optional,
            ),
            (
                "exposed, not certain (#802)",
                vec![import(|s| s.certain = false)],
                S::Exposed,
            ),
            (
                "deferred, optional elsewhere (#808)",
                vec![deferred, optional],
                S::DeferredOptional,
            ),
            (
                "a platform guard is no such evidence (#808)",
                vec![deferred, platform],
                S::Deferred,
            ),
            (
                "deferred, is_x predicate (#808)",
                vec![import(|s| (s.site, s.predicate) = (S::Deferred, true))],
                S::DeferredOptional,
            ),
            (
                "host loaded, even top level (#731)",
                vec![import(|s| s.host_loaded = true)],
                S::HostLoaded,
            ),
            (
                "opt-in, even top level (#781)",
                vec![import(|s| s.opt_in = true)],
                S::OptIn,
            ),
            (
                "management command, even top level (#1242)",
                vec![import(|s| s.command = true)],
                S::Command,
            ),
            (
                "a management command decides nothing against a top-level import",
                vec![import(|s| s.command = true), top],
                S::TopLevel,
            ),
            ("test support (#808)", vec![support(false)], S::TestSupport),
            (
                "test support the package loads itself",
                vec![support(true), support(false)],
                S::TopLevel,
            ),
        ];
        for (name, imports, expected) in cases {
            assert_eq!(settle(&imports), expected, "{name}");
        }
    }

    #[test]
    fn opt_in_imports_sit_under_example_dags_or_a_subpackage_named_after_them() {
        let layout = LayoutInfo::new(
            "",
            vec![
                "airflow".to_owned(),
                "dagster".to_owned(),
                "mlflow".to_owned(),
            ],
        );
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

    /// #1242: only the files of a Django app's `management/commands/`.
    #[test]
    fn management_commands_sit_in_an_apps_commands_directory() {
        assert!(is_management_command("hc/api/management/commands/prune.py"));
        assert!(is_management_command(
            "wger/core/management/commands/dummy-generator-users.py"
        ));
        assert!(is_management_command("management/commands/__init__.py"));
        assert!(!is_management_command("hc/api/management/commands"));
        assert!(!is_management_command("hc/api/management/utils.py"));
        assert!(!is_management_command("hc/commands/management/run.py"));
    }

    /// #1242: a subpackage named after a library one of its files imports
    /// covers its subpackages, and only those.
    #[test]
    fn integration_packages_cover_their_subpackages() {
        let layout = LayoutInfo::new("", vec!["mlflow".to_owned()]);
        let integrations = HashSet::from(["mlflow.genai.scorers.ragas".to_owned()]);
        let inside = |file: &str| in_integration(file, &layout, &integrations);
        assert!(inside("mlflow/genai/scorers/ragas/models.py"));
        assert!(inside("mlflow/genai/scorers/ragas/__init__.py"));
        assert!(inside("mlflow/genai/scorers/ragas/sub/run.py"));
        assert!(!inside("mlflow/genai/scorers/ragas.py"));
        assert!(!inside("mlflow/genai/scorers/ragas_utils/run.py"));
        assert!(!inside("mlflow/genai/scorers/base.py"));
    }

    /// #808: a `test`/`tests`/`testing` subpackage or module is test support,
    /// a top-level package of that name is not.
    #[test]
    fn test_support_sits_below_the_top_level_package() {
        let layout = LayoutInfo::new("", vec!["django".to_owned(), "testing".to_owned()]);
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
        let layout = LayoutInfo::new("src", vec!["acme".to_owned()]);
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
