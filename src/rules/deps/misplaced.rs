//! CHK005 misplaced dependency detection.

use std::collections::{HashMap, HashSet};

use crate::config::Confidence;
use crate::graph::ModuleOrigin;
use crate::parser::ParseSummary;
use crate::resolver::ResolvedImport;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};
use crate::rules::{DependencyRuleContext, RuleContext};

use super::context::{
    DeclarationBucket, UsageContext, declaration_bucket, declaration_buckets, include_path_details,
    is_directly_declared, usage_context_for_import,
};
use super::missing::{
    WorkspaceDeclaredIndex, collect_optional_imports, governing_declarations, member_declarations,
};
use super::used::DeclaredIndex;

/// Detect runtime usage of dev-only dependencies (and similar mismatches).
#[allow(clippy::too_many_lines)]
pub(super) fn detect_misplaced_dependencies(
    declared: &DeclaredIndex<'_>,
    dependency: &DependencyRuleContext<'_>,
    reachable: &HashSet<&str>,
    workspace_declared: &[WorkspaceDeclaredIndex<'_>],
) -> Vec<IssueCandidate> {
    let DependencyRuleContext {
        rules: context,
        config,
        strict,
    } = *dependency;
    let RuleContext {
        resolution,
        sources,
        ..
    } = *context;
    let strengths = import_strengths(context.parse);
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

        let strength = strengths
            .get(&(import.file.clone(), import.line))
            .copied()
            .unwrap_or(ImportStrength::TopLevel);
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
    /// `try`/`except ImportError` or platform-guarded.
    Optional,
    /// Only inside a function body.
    Deferred,
    TopLevel,
}

impl ImportStrength {
    fn apply(self, mut candidate: IssueCandidate) -> IssueCandidate {
        let detail = match self {
            Self::TopLevel => return candidate,
            Self::Optional => {
                candidate.severity = Severity::Info;
                "imported only under try/except ImportError or a platform guard"
            },
            Self::Deferred => "imported only inside functions",
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
        for import in &module.imports {
            let key = (module.path.clone(), import.line);
            let strength = if optional.contains(&key) {
                ImportStrength::Optional
            } else if import.deferred {
                ImportStrength::Deferred
            } else {
                ImportStrength::TopLevel
            };
            strengths.insert(key, strength);
        }
    }
    strengths
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
