//! YOK003 missing and YOK004 transitive dependency detection.

use std::collections::{HashSet, VecDeque};

use crate::config::{Confidence, YokeiConfig};
use crate::graph::ModuleOrigin;
use crate::parser::ParseSummary;
use crate::resolver::{ResolutionIndex, ResolvedImport, TransitiveIndex};
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};
use crate::sources::DiscoveredSources;

use super::context::{is_directly_declared, usage_context_for_import};
use super::used::DeclaredIndex;

/// Detect missing and transitive-only dependency imports.
#[allow(clippy::too_many_arguments)]
pub(super) fn detect_missing_dependencies(
    declared: &DeclaredIndex<'_>,
    resolution: &ResolutionIndex,
    reachable: &HashSet<String>,
    optional_imports: &HashSet<(String, u32)>,
    has_lockfile: bool,
    config: &YokeiConfig,
    sources: &DiscoveredSources,
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

        let key = (distribution.clone(), import.file.clone(), import.line);
        if !reported.insert(key) {
            continue;
        }

        let usage = usage_context_for_import(&import.file, import.context, sources);
        if declared
            .get(distribution)
            .is_some_and(|deps| is_directly_declared(deps, usage, config))
        {
            continue;
        }

        if optional_imports.contains(&(import.file.clone(), import.line)) {
            candidates.push(optional_missing_candidate(import, distribution, strict));
            continue;
        }

        if has_lockfile && is_transitive_only(distribution, declared, &resolution.transitive) {
            candidates.push(transitive_candidate(import, distribution));
            continue;
        }

        candidates.push(missing_candidate(import, distribution, has_lockfile));
    }

    candidates
}

fn optional_missing_candidate(
    import: &ResolvedImport,
    distribution: &str,
    strict: bool,
) -> IssueCandidate {
    let severity = if strict {
        Severity::Warning
    } else {
        Severity::Info
    };
    IssueCandidate {
        rule: RuleId::Yok003,
        subject: IssueSubject::Import {
            module: import.full_module.clone(),
            file: import.file.clone(),
            line: import.line,
        },
        severity,
        confidence: Confidence::Likely,
        message: format!(
            "optional try-import of {distribution} is not declared in any dependency context"
        ),
        origins: vec![Origin::Import {
            file: import.file.clone(),
            line: import.line,
            module: import.full_module.clone(),
        }],
        explain: ExplainData {
            summary: format!("optional import of {distribution} has no declaration"),
            details: vec![
                "try/except ImportError import — not treated as a hard missing dependency"
                    .to_owned(),
            ],
        },
    }
}

fn transitive_candidate(import: &ResolvedImport, distribution: &str) -> IssueCandidate {
    IssueCandidate {
        rule: RuleId::Yok004,
        subject: IssueSubject::Import {
            module: import.full_module.clone(),
            file: import.file.clone(),
            line: import.line,
        },
        severity: Severity::Error,
        confidence: Confidence::Certain,
        message: format!(
            "imported {distribution} directly but it is only available as a transitive dependency"
        ),
        origins: vec![Origin::Import {
            file: import.file.clone(),
            line: import.line,
            module: import.full_module.clone(),
        }],
        explain: ExplainData {
            summary: format!("{distribution} should be declared directly or import removed"),
            details: vec!["resolved via lockfile transitive closure".to_owned()],
        },
    }
}

fn missing_candidate(
    import: &ResolvedImport,
    distribution: &str,
    has_lockfile: bool,
) -> IssueCandidate {
    let lockfile_note = if has_lockfile {
        String::new()
    } else {
        " (no lockfile — transitive check skipped)".to_owned()
    };
    IssueCandidate {
        rule: RuleId::Yok003,
        subject: IssueSubject::Import {
            module: import.full_module.clone(),
            file: import.file.clone(),
            line: import.line,
        },
        severity: Severity::Error,
        confidence: Confidence::Certain,
        message: format!(
            "imported {distribution} in {}:{}{} but not declared in matching dependency context",
            import.file, import.line, lockfile_note
        ),
        origins: vec![Origin::Import {
            file: import.file.clone(),
            line: import.line,
            module: import.full_module.clone(),
        }],
        explain: ExplainData {
            summary: format!("{distribution} is imported but not declared"),
            details: vec![format!("import at {}:{}", import.file, import.line)],
        },
    }
}

/// Whether `distribution` appears in the transitive closure of declared direct deps.
#[must_use]
pub(super) fn is_transitive_only(
    distribution: &str,
    declared: &DeclaredIndex<'_>,
    transitive: &TransitiveIndex,
) -> bool {
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();

    for deps in declared.values() {
        for dep in deps {
            if visited.insert(dep.name.clone()) {
                queue.push_back(dep.name.clone());
            }
        }
    }

    while let Some(current) = queue.pop_front() {
        if current == distribution {
            return true;
        }
        if let Some(children) = transitive.edges.get(&current) {
            for child in children {
                if visited.insert(child.clone()) {
                    queue.push_back(child.clone());
                }
            }
        }
    }

    false
}

/// Build a set of optional try-import locations from parse output.
pub(super) fn collect_optional_imports(parse: &ParseSummary) -> HashSet<(String, u32)> {
    let mut optional = HashSet::new();
    for module in &parse.modules {
        for import in &module.imports {
            if import.optional {
                optional.insert((module.path.clone(), import.line));
            }
        }
    }
    optional
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::default_config;
    use crate::manifest::{DeclaredDependency, DependencyContext, DependencyOrigin};
    use crate::resolver::TransitiveIndex;

    fn declared_dep(name: &str) -> DeclaredDependency {
        DeclaredDependency {
            name: name.to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context: DependencyContext::Runtime,
            origin: DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(1),
                label: "project.dependencies[0]".to_owned(),
            },
            opaque: false,
        }
    }

    #[test]
    fn finds_transitive_dependency_in_lockfile_closure() {
        let requests = declared_dep("requests");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        let transitive = TransitiveIndex {
            edges: BTreeMap::from([("requests".to_owned(), vec!["urllib3".to_owned()])]),
        };
        assert!(is_transitive_only("urllib3", &index, &transitive));
        assert!(!is_transitive_only("certifi", &index, &transitive));
    }

    #[test]
    fn direct_declaration_skips_missing() {
        let config = default_config();
        let requests = declared_dep("requests");
        let mut index: DeclaredIndex<'_> = BTreeMap::new();
        index.insert("requests".to_owned(), vec![&requests]);
        let import = ResolvedImport {
            import_root: "requests".to_owned(),
            full_module: "requests".to_owned(),
            file: "src/app.py".to_owned(),
            line: 1,
            context: crate::parser::ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            origin: ModuleOrigin::ThirdParty,
            distribution: Some("requests".to_owned()),
            confidence: crate::resolver::ResolveConfidence::Certain,
        };
        let reachable = HashSet::from(["src/app.py".to_owned()]);
        let candidates = detect_missing_dependencies(
            &index,
            &ResolutionIndex {
                imports: vec![import],
                warnings: Vec::new(),
                transitive: TransitiveIndex::empty(),
                binary_resolutions: BTreeMap::new(),
            },
            &reachable,
            &HashSet::new(),
            true,
            &config,
            &crate::sources::DiscoveredSources {
                root: crate::discovery::ProjectRoot {
                    path: std::env::temp_dir(),
                    marker: crate::discovery::RootMarker::PyProjectToml,
                    start: std::env::temp_dir(),
                },
                layout: crate::sources::LayoutInfo {
                    layout: crate::sources::ProjectLayout::Src,
                    packages: vec!["acme".to_owned()],
                    inferred_globs: Vec::new(),
                    flat_candidates: Vec::new(),
                    ambiguous_flat_resolution: false,
                },
                effective_globs: Vec::new(),
                files: Vec::new(),
                warnings: Vec::new(),
            },
            false,
        );
        assert!(candidates.is_empty());
    }
}
