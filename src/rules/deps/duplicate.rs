//! CHK009 duplicate dependency declaration detection.
//!
//! A declaration is a duplicate when it repeats another one for the same
//! distribution either in the same context (the same list twice, with the
//! same marker) or in a group / extra while the distribution is already a
//! runtime dependency. Groups and extras do not duplicate each other: a
//! distribution needed by two extras is listed under both, and dependency
//! groups are installed independently (#494). A group or extra declaration
//! that adds extras the runtime declaration lacks (`streamlit[auth]` for a
//! runtime `streamlit`) refines it rather than repeating it (#507), and the
//! project's own self-referential extras (`all = ["pkg[a,b]"]`) are never
//! duplicates.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::Confidence;
use crate::manifest::{DeclaredDependency, DependencyContext, normalize_distribution_name};
use crate::rules::types::{ExplainData, IssueCandidate, IssueSubject, Origin, RuleId, Severity};

/// Detect the same distribution declared more than once where one declaration
/// makes the other redundant. `project_name` excludes the project's own
/// self-referential extras.
pub(super) fn detect_duplicate_dependencies(
    dependencies: &[DeclaredDependency],
    project_name: Option<&str>,
) -> Vec<IssueCandidate> {
    let project_name = project_name.map(normalize_distribution_name);
    let mut by_name: BTreeMap<&str, Vec<&DeclaredDependency>> = BTreeMap::new();
    for dep in dependencies {
        if dep.opaque || project_name.as_deref() == Some(dep.name.as_str()) {
            continue;
        }
        by_name.entry(dep.name.as_str()).or_default().push(dep);
    }

    by_name
        .into_iter()
        .filter_map(|(name, declarations)| {
            let involved = duplicate_declarations(&declarations);
            (!involved.is_empty()).then(|| candidate(name, &involved))
        })
        .collect()
}

fn duplicate_declarations<'a>(
    declarations: &[&'a DeclaredDependency],
) -> Vec<&'a DeclaredDependency> {
    let runtime_extras: BTreeSet<String> = declarations
        .iter()
        .filter(|dep| dep.context == DependencyContext::Runtime)
        .flat_map(|dep| {
            dep.extras
                .iter()
                .map(|extra| normalize_distribution_name(extra))
        })
        .collect();
    let mut involved = vec![false; declarations.len()];
    for (index, dep) in declarations.iter().enumerate() {
        for (other_index, other) in declarations.iter().enumerate().skip(index + 1) {
            if duplicates(dep, other, &runtime_extras) {
                involved[index] = true;
                involved[other_index] = true;
            }
        }
    }
    declarations
        .iter()
        .zip(involved)
        .filter_map(|(dep, involved)| involved.then_some(*dep))
        .collect()
}

fn duplicates(
    a: &DeclaredDependency,
    b: &DeclaredDependency,
    runtime_extras: &BTreeSet<String>,
) -> bool {
    // The same line reached twice (`requirements-dev.txt` including
    // `requirements.txt`, which is also read on its own) is one declaration,
    // even when the include gives it another context.
    if a.origin == b.origin {
        return false;
    }
    if a.context == b.context {
        return a.marker == b.marker;
    }
    let refinement = match (&a.context, &b.context) {
        (DependencyContext::Runtime, _) => b,
        (_, DependencyContext::Runtime) => a,
        _ => return false,
    };
    refinement
        .extras
        .iter()
        .all(|extra| runtime_extras.contains(&normalize_distribution_name(extra)))
}

fn candidate(name: &str, involved: &[&DeclaredDependency]) -> IssueCandidate {
    let labels: Vec<String> = involved
        .iter()
        .map(|dep| context_label(&dep.context))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let message = match labels.as_slice() {
        [label] => format!("{name} is declared more than once in {label}"),
        _ => format!(
            "{name} is declared in multiple contexts: {}",
            labels.join(", ")
        ),
    };
    IssueCandidate {
        rule: RuleId::Chk009,
        subject: IssueSubject::Distribution {
            name: name.to_owned(),
        },
        severity: Severity::Warning,
        confidence: Confidence::Certain,
        message,
        workspace_member: None,
        origins: involved
            .iter()
            .map(|dep| Origin::Manifest(dep.origin.clone()))
            .collect(),
        explain: ExplainData {
            summary: format!("{name} has duplicate declarations"),
            details: labels,
        },
    }
}

fn context_label(context: &DependencyContext) -> String {
    match context {
        DependencyContext::Runtime => "runtime".to_owned(),
        DependencyContext::Group(group) => format!("group:{group}"),
        DependencyContext::OptionalExtra(extra) | DependencyContext::SetupExtra(extra) => {
            format!("optional:{extra}")
        },
        DependencyContext::Build => "build".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::DependencyOrigin;

    /// A declaration on its own line: one manifest per context.
    fn dep(name: &str, context: DependencyContext) -> DeclaredDependency {
        let file = context_label(&context);
        at(name, context, &file, 1)
    }

    fn at(name: &str, context: DependencyContext, file: &str, line: u32) -> DeclaredDependency {
        DeclaredDependency {
            name: name.to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context,
            origin: DependencyOrigin {
                file: file.to_owned(),
                line: Some(line),
                label: name.to_owned(),
            },
            opaque: false,
            included_via: Vec::new(),
        }
    }

    fn with_extras(mut dep: DeclaredDependency, extras: &[&str]) -> DeclaredDependency {
        dep.extras = extras.iter().map(|extra| (*extra).to_owned()).collect();
        dep
    }

    fn with_marker(mut dep: DeclaredDependency, marker: &str) -> DeclaredDependency {
        dep.marker = Some(marker.to_owned());
        dep
    }

    fn group(name: &str) -> DependencyContext {
        DependencyContext::Group(name.to_owned())
    }

    fn extra(name: &str) -> DependencyContext {
        DependencyContext::OptionalExtra(name.to_owned())
    }

    fn detect(dependencies: &[DeclaredDependency]) -> Vec<IssueCandidate> {
        detect_duplicate_dependencies(dependencies, Some("Acme"))
    }

    fn messages(dependencies: &[DeclaredDependency]) -> Vec<String> {
        detect(dependencies)
            .into_iter()
            .map(|issue| issue.message)
            .collect()
    }

    #[test]
    fn reports_runtime_declaration_repeated_in_groups_and_extras() {
        let found = detect(&[
            dep("requests", DependencyContext::Runtime),
            dep("requests", group("dev")),
            dep("requests", group("typing")),
            dep("requests", extra("http")),
        ]);
        assert_eq!(found.len(), 1);
        let issue = &found[0];
        assert_eq!(issue.rule, RuleId::Chk009);
        assert_eq!(
            issue.subject,
            IssueSubject::Distribution {
                name: "requests".to_owned()
            }
        );
        assert_eq!(
            issue.message,
            "requests is declared in multiple contexts: group:dev, group:typing, optional:http, runtime"
        );
        assert_eq!(issue.origins.len(), 4);
        assert_eq!(
            issue.explain.details,
            ["group:dev", "group:typing", "optional:http", "runtime"]
        );
    }

    #[test]
    fn reports_declaration_repeated_in_the_same_context() {
        assert_eq!(
            messages(&[
                dep("requests", DependencyContext::Runtime),
                at("requests", DependencyContext::Runtime, "pyproject.toml", 2),
                dep("pytest", group("test")),
                at("pytest", group("test"), "pyproject.toml", 9),
                at("numpy", extra("fast"), "setup.py", 12),
                at("numpy", extra("fast"), "setup.cfg", 7),
            ]),
            [
                "numpy is declared more than once in optional:fast",
                "pytest is declared more than once in group:test",
                "requests is declared more than once in runtime",
            ]
        );
    }

    #[test]
    fn groups_and_extras_do_not_duplicate_each_other() {
        assert_eq!(
            messages(&[
                dep("boto3", extra("s3")),
                dep("boto3", extra("sqs")),
                dep("pytest", group("dev")),
                dep("pytest", group("test")),
                dep("mypy", group("typing")),
                dep("mypy", extra("lint")),
                dep("setuptools", DependencyContext::Build),
                dep("setuptools", group("dev")),
                dep("torch", DependencyContext::SetupExtra("all".to_owned())),
                dep("torch", DependencyContext::SetupExtra("torch".to_owned())),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn markers_opaque_and_self_references_are_not_duplicates() {
        assert_eq!(
            messages(&[
                with_marker(
                    dep("numpy", DependencyContext::Runtime),
                    "python_version < '3.12'"
                ),
                with_marker(
                    at("numpy", DependencyContext::Runtime, "runtime", 2),
                    "python_version >= '3.12'"
                ),
                dep("requests", DependencyContext::Runtime),
                DeclaredDependency {
                    opaque: true,
                    ..dep("requests", group("dev"))
                },
                with_extras(dep("acme", extra("all")), &["a", "b"]),
                dep("acme", DependencyContext::Runtime),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_same_line_read_through_two_manifests_is_one_declaration() {
        assert_eq!(
            messages(&[
                at("pytest", group("dev"), "requirements-dev.txt", 9),
                at("pytest", group("dev"), "requirements-dev.txt", 9),
                at(
                    "requests",
                    DependencyContext::Runtime,
                    "requirements.txt",
                    1
                ),
                at("requests", group("dev"), "requirements.txt", 1),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn group_declaration_adding_extras_refines_the_runtime_one() {
        let streamlit = with_extras(dep("streamlit", group("dev")), &["auth", "charts"]);
        assert_eq!(
            messages(&[dep("streamlit", DependencyContext::Runtime), streamlit]),
            Vec::<String>::new()
        );
        // The same extras again, in whichever spelling, add nothing.
        assert_eq!(
            messages(&[
                with_extras(dep("streamlit", DependencyContext::Runtime), &["Auth"]),
                with_extras(dep("streamlit", group("dev")), &["auth"]),
            ]),
            ["streamlit is declared in multiple contexts: group:dev, runtime"]
        );
    }

    #[test]
    fn only_the_duplicating_declarations_are_reported() {
        let found = detect(&[
            dep("kombu", DependencyContext::Runtime),
            with_extras(dep("kombu", extra("redis")), &["redis"]),
            dep("kombu", extra("plain")),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].message,
            "kombu is declared in multiple contexts: optional:plain, runtime"
        );
        assert_eq!(found[0].origins.len(), 2);
    }
}
