//! CHK002/CHK003 inside PEP 723 scripts, checked against the script block.

use std::collections::{BTreeSet, HashSet};

use crate::config::Confidence;
use crate::graph::ModuleOrigin;
use crate::manifest::{InlineScript, normalize_distribution_name};
use crate::resolver::{ResolutionIndex, ResolvedImport};
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};

/// Third-party imports made directly by a script file; project reconciliation
/// must not see them.
pub(super) fn is_script_third_party(import: &ResolvedImport, script_paths: &HashSet<&str>) -> bool {
    import.origin == ModuleOrigin::ThirdParty && script_paths.contains(import.file.as_str())
}

/// Per-script CHK002 and CHK003 candidates for reachable scripts.
pub(super) fn detect_script_dependency_issues(
    scripts: &[InlineScript],
    resolution: &ResolutionIndex,
    reachable: &HashSet<String>,
    strict: bool,
) -> Vec<IssueCandidate> {
    let mut candidates = Vec::new();
    for script in scripts
        .iter()
        .filter(|script| reachable.contains(&script.path))
    {
        let imports: Vec<&ResolvedImport> = resolution
            .imports
            .iter()
            .filter(|import| {
                import.file == script.path
                    && import.origin == ModuleOrigin::ThirdParty
                    && import.distribution.is_some()
            })
            .collect();
        let declared: BTreeSet<String> = script
            .dependencies
            .iter()
            .filter(|dep| !dep.opaque)
            .map(|dep| normalize_distribution_name(&dep.name))
            .collect();
        candidates.extend(missing_in_script(script, &imports, &declared));
        candidates.extend(unused_in_script(script, &imports, strict));
    }
    candidates
}

fn import_distribution(import: &ResolvedImport) -> Option<String> {
    import
        .distribution
        .as_deref()
        .map(normalize_distribution_name)
}

fn missing_in_script(
    script: &InlineScript,
    imports: &[&ResolvedImport],
    declared: &BTreeSet<String>,
) -> Vec<IssueCandidate> {
    let mut reported = BTreeSet::new();
    let mut candidates = Vec::new();
    for import in imports
        .iter()
        .filter(|import| !import.optional && !import.platform_guarded)
    {
        let Some(name) = import_distribution(import) else {
            continue;
        };
        if declared.contains(&name) || !reported.insert(name.clone()) {
            continue;
        }
        candidates.push(IssueCandidate {
            rule: RuleId::Chk003,
            subject: IssueSubject::ScriptDistribution {
                script: script.path.clone(),
                name: name.clone(),
            },
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: format!(
                "imported {name} in {}:{} but the PEP 723 script block does not declare it",
                import.file, import.line
            ),
            workspace_member: None,
            origins: vec![Origin::Import {
                file: import.file.clone(),
                line: import.line,
                module: import.full_module.clone(),
            }],
            explain: ExplainData {
                summary: format!("{name} is missing from the `# /// script` dependencies"),
                details: vec![
                    format!("import at {}:{}", import.file, import.line),
                    "script imports are checked against the script block, not the project manifest"
                        .to_owned(),
                ],
            },
        });
    }
    candidates
}

fn unused_in_script(
    script: &InlineScript,
    imports: &[&ResolvedImport],
    strict: bool,
) -> Vec<IssueCandidate> {
    let used: BTreeSet<String> = imports
        .iter()
        .filter_map(|import| import_distribution(import))
        .collect();
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for dep in &script.dependencies {
        let name = normalize_distribution_name(&dep.name);
        if dep.opaque
            || (!strict && dep.marker.is_some())
            || used.contains(&name)
            || !seen.insert(name.clone())
        {
            continue;
        }
        // Mirrors project CHK002: a marker may hide a platform-specific use.
        let (confidence, severity) = match (dep.marker.is_some(), strict) {
            (true, false) => (Confidence::Likely, Severity::Warning),
            (true, true) => (Confidence::Likely, Severity::Error),
            (false, _) => (Confidence::Certain, Severity::Error),
        };
        candidates.push(IssueCandidate {
            rule: RuleId::Chk002,
            subject: IssueSubject::ScriptDistribution {
                script: script.path.clone(),
                name: name.clone(),
            },
            severity,
            confidence,
            message: format!(
                "declared in the PEP 723 block of {}, no import in the script uses it",
                script.path
            ),
            workspace_member: None,
            origins: vec![Origin::Manifest(dep.origin.clone())],
            explain: ExplainData {
                summary: format!("{name} is declared by script {} but not used", script.path),
                details: vec![format!("declaration: {}", dep.origin.label)],
            },
        });
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{DeclaredDependency, DependencyContext, DependencyOrigin};
    use crate::parser::ImportContext;
    use crate::resolver::ResolveConfidence;

    fn dependency(name: &str, line: u32) -> DeclaredDependency {
        DeclaredDependency {
            name: name.to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context: DependencyContext::Runtime,
            origin: DependencyOrigin {
                file: "scripts/tool.py".to_owned(),
                line: Some(line),
                label: "script.dependencies[0]".to_owned(),
            },
            opaque: false,
        }
    }

    fn import(module: &str, distribution: &str, line: u32) -> ResolvedImport {
        ResolvedImport {
            import_root: module.to_owned(),
            full_module: module.to_owned(),
            file: "scripts/tool.py".to_owned(),
            workspace_member: None,
            line,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            origin: ModuleOrigin::ThirdParty,
            distribution: Some(distribution.to_owned()),
            confidence: ResolveConfidence::Certain,
        }
    }

    #[test]
    fn reports_missing_and_unused_script_dependencies() {
        let script = InlineScript {
            path: "scripts/tool.py".to_owned(),
            dependencies: vec![dependency("rich", 3), dependency("httpx", 4)],
            requires_python: None,
            target_version: None,
        };
        let resolution = ResolutionIndex {
            imports: vec![import("rich", "rich", 7), import("yaml", "pyyaml", 8)],
            ..ResolutionIndex::default()
        };
        let reachable = HashSet::from(["scripts/tool.py".to_owned()]);
        let found: Vec<_> =
            detect_script_dependency_issues(&[script], &resolution, &reachable, false)
                .into_iter()
                .map(|candidate| (candidate.rule, candidate.subject))
                .collect();
        let subject = |name: &str| IssueSubject::ScriptDistribution {
            script: "scripts/tool.py".to_owned(),
            name: name.to_owned(),
        };
        assert_eq!(
            found,
            [
                (RuleId::Chk003, subject("pyyaml")),
                (RuleId::Chk002, subject("httpx")),
            ]
        );
    }

    #[test]
    fn unreachable_scripts_are_not_checked() {
        let script = InlineScript {
            path: "scripts/tool.py".to_owned(),
            dependencies: vec![dependency("httpx", 3)],
            requires_python: None,
            target_version: None,
        };
        let found = detect_script_dependency_issues(
            &[script],
            &ResolutionIndex::default(),
            &HashSet::new(),
            false,
        );
        assert!(found.is_empty());
    }
}
