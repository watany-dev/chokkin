//! CHK005 misplaced dependency detection.

use std::collections::HashSet;

use crate::config::Confidence;
use crate::graph::ModuleOrigin;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};
use crate::rules::{DependencyRuleContext, RuleContext};

use super::context::{
    DeclarationBucket, UsageContext, declaration_bucket, declaration_buckets, include_path_details,
    usage_context_for_import,
};
use super::missing::{WorkspaceDeclaredIndex, governing_declarations};
use super::used::DeclaredIndex;

/// Detect runtime usage of dev-only dependencies (and similar mismatches).
#[allow(clippy::too_many_lines)]
pub(super) fn detect_misplaced_dependencies(
    declared: &DeclaredIndex<'_>,
    dependency: &DependencyRuleContext<'_>,
    reachable: &HashSet<String>,
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
    let mut candidates = Vec::new();
    let mut reported = HashSet::new();

    for import in &resolution.imports {
        if import.origin != ModuleOrigin::ThirdParty {
            continue;
        }
        let Some(distribution) = import.distribution.as_ref() else {
            continue;
        };
        if !reachable.contains(&import.file) {
            continue;
        }

        let usage = usage_context_for_import(&import.file, import.context, sources);
        if usage != UsageContext::Runtime {
            continue;
        }

        let workspace_member = import.workspace_member.as_deref();
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

        let report_key = (
            workspace_member.unwrap_or_default().to_owned(),
            distribution.clone(),
        );
        if !reported.insert(report_key) {
            continue;
        }

        let contexts: Vec<String> = declarations
            .iter()
            .map(|dep| declaration_bucket(&dep.context, &config.dependencies).label())
            .collect();

        candidates.push(IssueCandidate {
            rule: RuleId::Chk005,
            subject: IssueSubject::Distribution {
                name: distribution.clone(),
            },
            severity: Severity::Warning,
            confidence: Confidence::Certain,
            message: misplaced_message(distribution, workspace_member, &contexts),
            workspace_member: workspace_member.map(str::to_owned),
            origins: vec![Origin::Import {
                file: import.file.clone(),
                line: import.line,
                module: import.full_module.clone(),
            }],
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
        });
    }

    candidates
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
