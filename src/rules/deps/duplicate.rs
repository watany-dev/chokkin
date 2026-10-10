//! CHK009 duplicate dependency declaration detection.
//!
//! A declaration is a duplicate when another one for the same distribution
//! already implies it: the same requirement repeated in the same context, or
//! a group / extra declaration that the runtime declaration fully covers.
//! Groups and extras do not duplicate each other: a distribution needed by two
//! extras is listed under both, and dependency groups are installed
//! independently (#494). A group or extra declaration that adds extras the
//! runtime declaration lacks (`streamlit[auth]` for a runtime `streamlit`)
//! refines it rather than repeating it (#507), and so does one with its own
//! marker or version specifier (`click!=8.3.0` for a runtime `click>=7`):
//! removing it would change what gets installed (#629). The project's own
//! self-referential extras (`all = ["pkg[a,b]"]`) are never duplicates.
//!
//! Only a repeat in the same context is a warning `--fix` removes. A group or
//! extra repeating the runtime declaration is info / likely: a group may be
//! installed on its own (`uv sync --only-group lint`) and generated extras
//! mirror another package's, which static analysis cannot tell apart (#696).

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
            // A repeat in one context is reported on its own, so the warning
            // names only what `--fix` removes.
            let removable = removable_duplicates(&declarations);
            let (involved, removable) = if removable.is_empty() {
                (duplicate_declarations(&declarations, |_, _| true), false)
            } else {
                (removable, true)
            };
            (!involved.is_empty()).then(|| candidate(name, &involved, removable))
        })
        .collect()
}

/// The declarations of one distribution that repeat another one in the same
/// context; `--fix` removes from these only (#696).
pub(crate) fn removable_duplicates<'a>(
    declarations: &[&'a DeclaredDependency],
) -> Vec<&'a DeclaredDependency> {
    duplicate_declarations(declarations, |a, b| a.context == b.context)
}

fn duplicate_declarations<'a>(
    declarations: &[&'a DeclaredDependency],
    pair: impl Fn(&DeclaredDependency, &DeclaredDependency) -> bool,
) -> Vec<&'a DeclaredDependency> {
    declarations
        .iter()
        .copied()
        .filter(|dep| {
            declarations
                .iter()
                .any(|other| pair(dep, other) && duplicates(dep, other, declarations))
        })
        .collect()
}

fn duplicates(
    a: &DeclaredDependency,
    b: &DeclaredDependency,
    declarations: &[&DeclaredDependency],
) -> bool {
    // The same line reached twice (`requirements-dev.txt` including
    // `requirements.txt`, which is also read on its own) is one declaration,
    // even when the include gives it another context. This also keeps a
    // declaration from duplicating itself.
    if a.origin == b.origin || a.marker != b.marker {
        return false;
    }
    if a.context == b.context {
        return specifier_set(a) == specifier_set(b) && extra_set(a) == extra_set(b);
    }
    let (runtime, refinement) = match (&a.context, &b.context) {
        (DependencyContext::Runtime, _) => (a, b),
        (_, DependencyContext::Runtime) => (b, a),
        _ => return false,
    };
    // No PEP 440 containment check: only a bare or identical specifier is
    // known to add nothing to the runtime one.
    let specifier = specifier_set(refinement);
    if !specifier.is_empty() && specifier != specifier_set(runtime) {
        return false;
    }
    // Extras may be spread over several runtime declarations, but only ones
    // installed under the same marker cover the refinement.
    let runtime_extras: BTreeSet<String> = declarations
        .iter()
        .filter(|dep| dep.context == DependencyContext::Runtime && dep.marker == refinement.marker)
        .flat_map(|dep| extra_set(dep))
        .collect();
    extra_set(refinement).is_subset(&runtime_extras)
}

fn extra_set(dep: &DeclaredDependency) -> BTreeSet<String> {
    dep.extras
        .iter()
        .map(|extra| normalize_distribution_name(extra))
        .collect()
}

/// `<9, >=7.0` and `>=7.0,<9` are the same constraint.
fn specifier_set(dep: &DeclaredDependency) -> BTreeSet<&str> {
    dep.specifier
        .iter()
        .flat_map(|specifier| specifier.split(','))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

fn candidate(name: &str, involved: &[&DeclaredDependency], removable: bool) -> IssueCandidate {
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
    let (severity, confidence) = if removable {
        (Severity::Warning, Confidence::Certain)
    } else {
        (Severity::Info, Confidence::Likely)
    };
    IssueCandidate {
        rule: RuleId::Chk009,
        subject: IssueSubject::Distribution {
            name: name.to_owned(),
        },
        severity,
        confidence,
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

    fn with_specifier(mut dep: DeclaredDependency, specifier: &str) -> DeclaredDependency {
        dep.specifier = Some(specifier.to_owned());
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
        // #696: a group may be installed on its own, so this is only a hint.
        assert_eq!(issue.severity, Severity::Info);
        assert_eq!(issue.confidence, Confidence::Likely);
    }

    #[test]
    fn reports_declaration_repeated_in_the_same_context() {
        let dependencies = [
            dep("requests", DependencyContext::Runtime),
            at("requests", DependencyContext::Runtime, "pyproject.toml", 2),
            dep("pytest", group("test")),
            at("pytest", group("test"), "pyproject.toml", 9),
            at("numpy", extra("fast"), "setup.py", 12),
            at("numpy", extra("fast"), "setup.cfg", 7),
        ];
        assert_eq!(
            messages(&dependencies),
            [
                "numpy is declared more than once in optional:fast",
                "pytest is declared more than once in group:test",
                "requests is declared more than once in runtime",
            ]
        );
        assert!(detect(&dependencies).iter().all(|issue| {
            issue.severity == Severity::Warning && issue.confidence == Confidence::Certain
        }));
    }

    #[test]
    fn a_repeat_in_one_context_is_reported_without_the_cross_context_one() {
        let found = detect(&[
            dep("requests", DependencyContext::Runtime),
            at("requests", DependencyContext::Runtime, "pyproject.toml", 2),
            dep("requests", group("lint")),
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].message,
            "requests is declared more than once in runtime"
        );
        assert_eq!(found[0].severity, Severity::Warning);
        assert_eq!(found[0].origins.len(), 2);
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
    fn extras_must_match_in_the_same_context_and_marker() {
        assert_eq!(
            messages(&[
                dep("httpx", group("dev")),
                with_extras(at("httpx", group("dev"), "pyproject.toml", 9), &["http2"]),
                dep("celery", DependencyContext::Runtime),
                with_marker(
                    with_extras(
                        at("celery", DependencyContext::Runtime, "runtime", 2),
                        &["redis"]
                    ),
                    "sys_platform == 'linux'"
                ),
                with_extras(dep("celery", group("dev")), &["redis"]),
            ]),
            Vec::<String>::new()
        );
        // Extras spread over runtime declarations under one marker still cover.
        assert_eq!(
            messages(&[
                with_extras(dep("kombu", DependencyContext::Runtime), &["redis"]),
                with_extras(
                    at("kombu", DependencyContext::Runtime, "runtime", 2),
                    &["sqs"]
                ),
                with_extras(dep("kombu", group("dev")), &["redis", "sqs"]),
            ]),
            ["kombu is declared in multiple contexts: group:dev, runtime"]
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

    #[test]
    fn group_or_extra_with_its_own_constraint_is_not_a_duplicate() {
        // #629: each of these changes what gets installed when removed.
        assert_eq!(
            messages(&[
                with_specifier(dep("click", DependencyContext::Runtime), ">=7"),
                with_specifier(dep("click", extra("mcp")), "!=8.3.0"),
                with_marker(
                    with_specifier(dep("requests", DependencyContext::Runtime), ">=2"),
                    "python_version >= '3.10'"
                ),
                with_specifier(dep("requests", extra("legacy")), ">=2"),
                dep("rich", DependencyContext::Runtime),
                with_specifier(dep("rich", group("lint")), ">=13.0"),
                with_specifier(dep("pytest", group("test")), ">=7"),
                with_specifier(at("pytest", group("test"), "pyproject.toml", 9), ">=8"),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn bare_or_identical_specifier_still_duplicates_the_runtime_one() {
        assert_eq!(
            messages(&[
                with_specifier(dep("fastapi", DependencyContext::Runtime), "<1"),
                with_specifier(dep("fastapi", extra("genai")), "<1"),
                with_specifier(dep("click", DependencyContext::Runtime), "<9, >=7.0"),
                with_specifier(dep("click", group("dev")), ">=7.0,<9"),
                with_specifier(dep("aiohttp", DependencyContext::Runtime), "<4"),
                dep("aiohttp", extra("http")),
                with_marker(
                    dep("numpy", DependencyContext::Runtime),
                    "sys_platform == 'linux'"
                ),
                with_marker(dep("numpy", group("dev")), "sys_platform == 'linux'"),
            ]),
            [
                "aiohttp is declared in multiple contexts: optional:http, runtime",
                "click is declared in multiple contexts: group:dev, runtime",
                "fastapi is declared in multiple contexts: optional:genai, runtime",
                "numpy is declared in multiple contexts: group:dev, runtime",
            ]
        );
    }
}
