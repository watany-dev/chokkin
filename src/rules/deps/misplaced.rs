//! CHK005 misplaced dependency detection.

use std::collections::HashSet;

use crate::config::{ChokkinConfig, Confidence};
use crate::graph::ModuleOrigin;
use crate::resolver::ResolutionIndex;
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};
use crate::sources::DiscoveredSources;

use super::context::{
    DeclarationBucket, UsageContext, declaration_bucket, usage_context_for_import,
};
use super::missing::WorkspaceDeclaredIndex;
use super::used::DeclaredIndex;

/// Detect runtime usage of dev-only dependencies (and similar mismatches).
#[allow(clippy::too_many_arguments)]
pub(super) fn detect_misplaced_dependencies(
    declared: &DeclaredIndex<'_>,
    resolution: &ResolutionIndex,
    reachable: &HashSet<String>,
    config: &ChokkinConfig,
    sources: &DiscoveredSources,
    workspace_declared: &[WorkspaceDeclaredIndex<'_>],
    strict: bool,
) -> Vec<IssueCandidate> {
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
        let declarations = workspace_member
            .filter(|_| strict)
            .and_then(|member_id| {
                workspace_declared
                    .iter()
                    .find(|boundary| boundary.member_id == member_id)
                    .and_then(|boundary| boundary.declared.get(distribution))
            })
            .or_else(|| {
                if strict && workspace_member.is_some() {
                    None
                } else {
                    declared.get(distribution)
                }
            });
        let Some(declarations) = declarations else {
            continue;
        };

        let has_runtime = declarations.iter().any(|dep| {
            matches!(
                declaration_bucket(&dep.context, &config.dependencies),
                DeclarationBucket::Runtime | DeclarationBucket::Optional(_)
            )
        });
        if has_runtime {
            continue;
        }

        let has_dev_only = declarations.iter().any(|dep| {
            matches!(
                declaration_bucket(&dep.context, &config.dependencies),
                DeclarationBucket::Dev | DeclarationBucket::Type
            )
        });
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
            origins: vec![Origin::Import {
                file: import.file.clone(),
                line: import.line,
                module: import.full_module.clone(),
            }],
            explain: ExplainData {
                summary: format!("{distribution} is misplaced for runtime usage"),
                details: vec![format!("declared in: {}", contexts.join(", "))],
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
