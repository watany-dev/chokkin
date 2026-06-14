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
use super::used::DeclaredIndex;

/// Detect runtime usage of dev-only dependencies (and similar mismatches).
pub(super) fn detect_misplaced_dependencies(
    declared: &DeclaredIndex<'_>,
    resolution: &ResolutionIndex,
    reachable: &HashSet<String>,
    config: &ChokkinConfig,
    sources: &DiscoveredSources,
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

        let Some(declarations) = declared.get(distribution) else {
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

        if !reported.insert(distribution.clone()) {
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
            message: format!(
                "{distribution} is used from runtime code but only declared in {}",
                contexts.join(", ")
            ),
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
