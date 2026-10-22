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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::config::Confidence;
use crate::manifest::{
    DeclaredDependency, DependencyContext, DependencyOrigin, normalize_distribution_name,
};
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
            let prepared = prepare(&declarations);
            let repeated = repeated_in_context(&prepared);
            // A repeat in one context is reported on its own, located on the
            // line `--fix` removes.
            let (involved, removable) =
                if let Some(to_remove) = pick_removal(&declarations, &repeated) {
                    let mut involved = select(&declarations, &repeated);
                    involved.sort_by_key(|dep| std::ptr::eq(*dep, to_remove));
                    (involved, true)
                } else {
                    let either: Vec<bool> = repeated
                        .iter()
                        .zip(covered_by_runtime(&prepared))
                        .map(|(repeated, covered)| *repeated || covered)
                        .collect();
                    (select(&declarations, &either), false)
                };
            (!involved.is_empty()).then(|| candidate(name, &involved, removable))
        })
        .collect()
}

pub(crate) fn duplicate_to_remove<'a>(
    declarations: &[&'a DeclaredDependency],
) -> Option<&'a DeclaredDependency> {
    pick_removal(declarations, &repeated_in_context(&prepare(declarations)))
}

fn pick_removal<'a>(
    declarations: &[&'a DeclaredDependency],
    repeated: &[bool],
) -> Option<&'a DeclaredDependency> {
    select(declarations, &removable_among(declarations, repeated))
        .into_iter()
        .max_by_key(|dep| removal_priority(&dep.context))
}

fn removal_priority(context: &DependencyContext) -> u8 {
    match context {
        DependencyContext::Runtime => 0,
        DependencyContext::OptionalExtra(_) => 1,
        DependencyContext::Group(_) => 2,
        DependencyContext::SetupExtra(_) => 3,
        DependencyContext::Build => 4,
    }
}

/// The declarations `mask` marks, in declaration order.
fn select<'a>(
    declarations: &[&'a DeclaredDependency],
    mask: &[bool],
) -> Vec<&'a DeclaredDependency> {
    declarations
        .iter()
        .zip(mask)
        .filter_map(|(dep, marked)| marked.then_some(*dep))
        .collect()
}

/// One declaration with the sets the duplicate checks compare, built once per
/// declaration instead of once per pair (#1048).
struct Prepared<'a> {
    dep: &'a DeclaredDependency,
    specifiers: BTreeSet<&'a str>,
    extras: BTreeSet<String>,
}

fn prepare<'a>(declarations: &[&'a DeclaredDependency]) -> Vec<Prepared<'a>> {
    declarations
        .iter()
        .map(|dep| Prepared {
            dep,
            specifiers: specifier_set(dep),
            extras: extra_set(dep),
        })
        .collect()
}

/// Up to two distinct origins: enough to tell whether a bucket holds a
/// declaration on a line other than a given one.
#[derive(Default)]
struct Origins<'a>([Option<&'a DependencyOrigin>; 2]);

impl<'a> Origins<'a> {
    fn add(&mut self, origin: &'a DependencyOrigin) {
        match self.0 {
            [None, _] => self.0[0] = Some(origin),
            [Some(first), None] if first != origin => self.0[1] = Some(origin),
            _ => {},
        }
    }

    fn other_than(&self, origin: &DependencyOrigin) -> bool {
        self.0.iter().flatten().any(|known| *known != origin)
    }
}

/// Which declarations of one distribution `--fix` may remove: ones that
/// repeat another one in the same context (#696). `--fix` removes the line,
/// so a line also read under another context (`requirements.txt` pulled into
/// `requirements-dev.txt` by `-r`) qualifies only when it is repeated in
/// every context that reads it (#860).
fn removable_among(declarations: &[&DeclaredDependency], repeated: &[bool]) -> Vec<bool> {
    let mut every_read_repeated: HashMap<&DependencyOrigin, bool> = HashMap::new();
    for (dep, repeated) in declarations.iter().zip(repeated) {
        *every_read_repeated.entry(&dep.origin).or_insert(true) &= *repeated;
    }
    declarations
        .iter()
        .zip(repeated)
        .map(|(dep, repeated)| {
            *repeated
                && every_read_repeated
                    .get(&dep.origin)
                    .copied()
                    .unwrap_or(false)
        })
        .collect()
}

type ContextKey<'k> = (
    &'k DependencyContext,
    Option<&'k str>,
    &'k BTreeSet<&'k str>,
    &'k BTreeSet<String>,
);

fn context_key<'k>(item: &'k Prepared<'_>) -> ContextKey<'k> {
    (
        &item.dep.context,
        item.dep.marker.as_deref(),
        &item.specifiers,
        &item.extras,
    )
}

fn marker<'k>(item: &'k Prepared<'_>) -> Option<&'k str> {
    item.dep.marker.as_deref()
}

/// Which declarations repeat another one in the same context: the same
/// marker, specifiers and extras on another line. The same line reached
/// twice (`requirements-dev.txt` including `requirements.txt`, which is also
/// read on its own) is one declaration, even when the include gives it
/// another context; this also keeps a declaration from repeating itself.
fn repeated_in_context(prepared: &[Prepared<'_>]) -> Vec<bool> {
    let mut buckets: HashMap<ContextKey<'_>, Origins<'_>> = HashMap::new();
    for item in prepared {
        buckets
            .entry(context_key(item))
            .or_default()
            .add(&item.dep.origin);
    }
    prepared
        .iter()
        .map(|item| {
            buckets
                .get(&context_key(item))
                .is_some_and(|origins| origins.other_than(&item.dep.origin))
        })
        .collect()
}

/// Which declarations take part in a group or extra declaration that a
/// runtime one covers, on another line under the same marker. No PEP 440
/// containment check: only a bare or identical specifier is known to add
/// nothing to the runtime one. Extras may be spread over several runtime
/// declarations, but only ones installed under the same marker cover the
/// refinement.
fn covered_by_runtime(prepared: &[Prepared<'_>]) -> Vec<bool> {
    type SpecKey<'k> = (Option<&'k str>, &'k BTreeSet<&'k str>);
    let is_runtime = |item: &Prepared<'_>| item.dep.context == DependencyContext::Runtime;

    let mut runtime_extras: HashMap<Option<&str>, BTreeSet<&str>> = HashMap::new();
    let mut runtime_any: HashMap<Option<&str>, Origins<'_>> = HashMap::new();
    let mut runtime_by_spec: HashMap<SpecKey<'_>, Origins<'_>> = HashMap::new();
    for item in prepared.iter().filter(|item| is_runtime(item)) {
        runtime_extras
            .entry(marker(item))
            .or_default()
            .extend(item.extras.iter().map(String::as_str));
        runtime_any
            .entry(marker(item))
            .or_default()
            .add(&item.dep.origin);
        runtime_by_spec
            .entry((marker(item), &item.specifiers))
            .or_default()
            .add(&item.dep.origin);
    }

    let covered_extras = |item: &Prepared<'_>| {
        runtime_extras
            .get(&marker(item))
            .map_or(item.extras.is_empty(), |runtime| {
                item.extras
                    .iter()
                    .all(|extra| runtime.contains(extra.as_str()))
            })
    };
    // Refinements whose extras the runtime declarations cover; a bare one
    // matches every runtime specifier.
    let mut bare_refinements: HashMap<Option<&str>, Origins<'_>> = HashMap::new();
    let mut refinements_by_spec: HashMap<SpecKey<'_>, Origins<'_>> = HashMap::new();
    let mut covered = vec![false; prepared.len()];
    for (item, covered) in prepared.iter().zip(covered.iter_mut()) {
        if is_runtime(item) || !covered_extras(item) {
            continue;
        }
        let runtime = if item.specifiers.is_empty() {
            bare_refinements
                .entry(marker(item))
                .or_default()
                .add(&item.dep.origin);
            runtime_any.get(&marker(item))
        } else {
            refinements_by_spec
                .entry((marker(item), &item.specifiers))
                .or_default()
                .add(&item.dep.origin);
            runtime_by_spec.get(&(marker(item), &item.specifiers))
        };
        *covered = runtime.is_some_and(|runtime| runtime.other_than(&item.dep.origin));
    }
    for (item, covered) in prepared.iter().zip(covered.iter_mut()) {
        if is_runtime(item) {
            let origin = &item.dep.origin;
            *covered = bare_refinements
                .get(&marker(item))
                .is_some_and(|refinements| refinements.other_than(origin))
                || refinements_by_spec
                    .get(&(marker(item), &item.specifiers))
                    .is_some_and(|refinements| refinements.other_than(origin));
        }
    }
    covered
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
            unportable_key: None,
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

    /// The pairwise definition the bucketed detection implements: a
    /// declaration is involved when some other one duplicates it. Quadratic
    /// to cubic in the declarations of one distribution (#1048), so only the
    /// tests run it, to check the detection against it.
    fn duplicates(
        a: &DeclaredDependency,
        b: &DeclaredDependency,
        declarations: &[&DeclaredDependency],
    ) -> bool {
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
        let specifier = specifier_set(refinement);
        if !specifier.is_empty() && specifier != specifier_set(runtime) {
            return false;
        }
        let runtime_extras: BTreeSet<String> = declarations
            .iter()
            .filter(|dep| {
                dep.context == DependencyContext::Runtime && dep.marker == refinement.marker
            })
            .flat_map(|dep| extra_set(dep))
            .collect();
        extra_set(refinement).is_subset(&runtime_extras)
    }

    fn pairwise_involved(
        declarations: &[&DeclaredDependency],
        pair: impl Fn(&DeclaredDependency, &DeclaredDependency) -> bool,
    ) -> Vec<bool> {
        declarations
            .iter()
            .map(|dep| {
                declarations
                    .iter()
                    .any(|other| pair(dep, other) && duplicates(dep, other, declarations))
            })
            .collect()
    }

    /// `(repeated in one context, involved in any duplicate)` by the
    /// pairwise definition.
    fn pairwise(declarations: &[&DeclaredDependency]) -> (Vec<bool>, Vec<bool>) {
        (
            pairwise_involved(declarations, |a, b| a.context == b.context),
            pairwise_involved(declarations, |_, _| true),
        )
    }

    fn removable_duplicates<'a>(
        declarations: &[&'a DeclaredDependency],
    ) -> Vec<&'a DeclaredDependency> {
        let repeated = repeated_in_context(&prepare(declarations));
        select(declarations, &removable_among(declarations, &repeated))
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
    fn many_declarations_of_one_distribution_stay_cheap() {
        // #1048: pairing every declaration with every other one, and
        // rebuilding the runtime extras for each pair, took seconds for a few
        // thousand; bucketing keeps this test fast.
        let mut dependencies: Vec<DeclaredDependency> = (0..20_000)
            .map(|line| {
                let runtime = at("foo", DependencyContext::Runtime, "pyproject.toml", line);
                with_specifier(runtime, &format!("=={line}"))
            })
            .collect();
        dependencies.extend((0..20_000).map(|line| {
            with_extras(
                at("foo", group("dev"), "pyproject.toml", 30_000 + line),
                &[&format!("x{line}")],
            )
        }));
        dependencies.push(at("foo", group("dev"), "pyproject.toml", 60_000));
        let found = detect(&dependencies);
        assert_eq!(found.len(), 1);
        // Only the bare group declaration repeats the runtime ones.
        assert_eq!(found[0].origins.len(), 20_001);
        assert_eq!(found[0].severity, Severity::Info);
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
    fn a_line_read_under_two_contexts_is_removable_only_if_repeated_in_both() {
        // `requirements.txt` and `requirements-dev.txt` both have
        // `-r base.txt`; removing `base.txt:1` for the repeat in
        // `requirements.txt` would drop it from the dev install.
        let base = at("requests", DependencyContext::Runtime, "base.txt", 1);
        let included = DeclaredDependency {
            context: group("dev"),
            ..base.clone()
        };
        let requirements = at(
            "requests",
            DependencyContext::Runtime,
            "requirements.txt",
            1,
        );
        let found = detect(&[base.clone(), included.clone(), requirements.clone()]);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].message,
            "requests is declared more than once in runtime"
        );
        assert_eq!(found[0].severity, Severity::Warning);
        assert_eq!(found[0].origins.len(), 2);
        assert_eq!(
            removable_duplicates(&[&base, &included, &requirements]),
            [&requirements]
        );
        // The issue sits on the line `--fix` removes, whatever the load order.
        let found = detect(&[requirements.clone(), base.clone(), included.clone()]);
        assert_eq!(
            found[0].origins.last(),
            Some(&Origin::Manifest(requirements.origin.clone()))
        );
        // Repeated in the dev install too, the line is safe to remove.
        let dev = at("requests", group("dev"), "requirements-dev.txt", 3);
        assert_eq!(
            removable_duplicates(&[&base, &included, &requirements, &dev]).len(),
            4
        );
    }

    #[test]
    fn the_issue_sits_on_the_line_fix_removes_across_contexts() {
        let dev = [
            at("requests", group("dev"), "requirements-dev.txt", 1),
            at("requests", group("dev"), "requirements-dev.txt", 2),
        ];
        let runtime = [
            at(
                "requests",
                DependencyContext::Runtime,
                "requirements.txt",
                1,
            ),
            at(
                "requests",
                DependencyContext::Runtime,
                "requirements.txt",
                2,
            ),
        ];
        for declarations in [
            [&dev[0], &dev[1], &runtime[0], &runtime[1]],
            [&runtime[0], &runtime[1], &dev[0], &dev[1]],
        ] {
            let owned: Vec<DeclaredDependency> =
                declarations.iter().map(|dep| (*dep).clone()).collect();
            let removed = duplicate_to_remove(&declarations).map(|dep| &dep.origin);
            assert_eq!(removed, Some(&dev[1].origin));
            assert_eq!(
                detect(&owned)[0].origins.last(),
                Some(&Origin::Manifest(dev[1].origin.clone()))
            );
        }
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

    /// What one install ends up with for a distribution: whether it is
    /// installed, with which extras, under which version clauses. A reference
    /// model of pip / uv, written apart from `duplicates` (#860): extras
    /// compare case-insensitively, clauses as written minus whitespace.
    #[derive(Debug, PartialEq, Eq)]
    struct Installed {
        present: bool,
        extras: BTreeSet<String>,
        clauses: BTreeSet<String>,
    }

    /// Install the declarations whose context is selected, on a platform
    /// where the one marker of the model is `linux` or not.
    fn install(
        declarations: &[&DeclaredDependency],
        selection: &[&DependencyContext],
        linux: bool,
    ) -> Installed {
        let active = declarations
            .iter()
            .filter(|dep| selection.contains(&&dep.context) && (dep.marker.is_none() || linux));
        let mut installed = Installed {
            present: false,
            extras: BTreeSet::new(),
            clauses: BTreeSet::new(),
        };
        for dep in active {
            installed.present = true;
            installed
                .extras
                .extend(dep.extras.iter().map(|extra| extra.to_lowercase()));
            installed.clauses.extend(
                dep.specifier
                    .iter()
                    .flat_map(|specifier| specifier.split(','))
                    .map(|clause| clause.replace(' ', ""))
                    .filter(|clause| !clause.is_empty()),
            );
        }
        installed
    }

    /// Every install a project offers over `contexts`: any set of contexts
    /// (`uv sync --only-group lint`, `pip install -r requirements-dev.txt`),
    /// on either side of the marker.
    fn installs_agree(
        before: &[&DeclaredDependency],
        after: &[&DeclaredDependency],
        contexts: &[DependencyContext],
        must_include_runtime: bool,
    ) -> bool {
        (0..1_u32 << contexts.len()).all(|mask| {
            let selection: Vec<&DependencyContext> = contexts
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, context)| context)
                .collect();
            if must_include_runtime && !selection.contains(&&DependencyContext::Runtime) {
                return true;
            }
            [false, true].into_iter().all(|linux| {
                install(before, &selection, linux) == install(after, &selection, linux)
            })
        })
    }

    fn requirements(
        contexts: &[DependencyContext],
        specifiers: &[Option<&str>],
        extras: &[&[&str]],
    ) -> Vec<DeclaredDependency> {
        let mut all = Vec::new();
        for context in contexts {
            for marker in [None, Some("sys_platform == 'linux'")] {
                for specifier in specifiers {
                    for extra_list in extras {
                        let mut dep = dep("pkg", context.clone());
                        dep.marker = marker.map(str::to_owned);
                        dep.specifier = specifier.map(str::to_owned);
                        dep.extras = extra_list.iter().map(|e| (*e).to_owned()).collect();
                        all.push(dep);
                    }
                }
            }
        }
        all
    }

    /// The properties #860 asks of CHK009, over three declarations of one
    /// distribution: every combination of context, marker, specifier and
    /// extras, each on its own line or one line read through an include under
    /// a second context.
    ///
    /// - `duplicates` is symmetric.
    /// - `--fix` removes a declaration's line; for anything
    ///   `removable_duplicates` returns, that changes no install, including
    ///   one of a single group.
    /// - A group or extra that duplicates the runtime declaration adds nothing
    ///   to an install that has the runtime one (#507, #629).
    #[test]
    fn duplicates_follow_the_install_model() {
        let contexts = [DependencyContext::Runtime, group("dev"), extra("x")];
        let lines = requirements(
            &contexts,
            &[None, Some(">=1"), Some("<2, >=1")],
            &[&[], &["a"], &["b"], &["A", "b"]],
        );
        let on = |file: &str, line: u32| -> Vec<DeclaredDependency> {
            lines
                .iter()
                .map(|dep| DeclaredDependency {
                    origin: DependencyOrigin {
                        file: file.to_owned(),
                        line: Some(line),
                        ..dep.origin.clone()
                    },
                    ..dep.clone()
                })
                .collect()
        };
        let (first, second, third) = (
            on("pyproject.toml", 1),
            on("pyproject.toml", 2),
            on("pyproject.toml", 3),
        );
        // Which line holds which requirement does not matter, so each
        // multiset of three requirements is checked once.
        for (i, a) in first.iter().enumerate() {
            for (j, b) in second.iter().enumerate().skip(i) {
                for c in &third[j..] {
                    check_triple(&[a, b, c], &contexts);
                }
            }
        }
        // `base.txt` read through `-r` from files of two contexts, next to
        // a line of its own.
        for a in on("base.txt", 1) {
            for context in contexts.iter().filter(|context| **context != a.context) {
                let included = DeclaredDependency {
                    context: context.clone(),
                    ..a.clone()
                };
                for b in &first {
                    check_triple(&[&a, &included, b], &contexts);
                    check_triple(&[b, &a, &included], &contexts);
                }
            }
        }
    }

    fn check_triple(declarations: &[&DeclaredDependency], contexts: &[DependencyContext]) {
        for (i, a) in declarations.iter().enumerate() {
            for b in &declarations[i + 1..] {
                let duplicate = duplicates(a, b, declarations);
                assert_eq!(
                    duplicate,
                    duplicates(b, a, declarations),
                    "symmetry: {a:?} {b:?}"
                );
                if !duplicate || a.context == b.context {
                    continue;
                }
                let refinement = if a.context == DependencyContext::Runtime {
                    b
                } else {
                    a
                };
                let rest: Vec<&DeclaredDependency> = declarations
                    .iter()
                    .copied()
                    .filter(|dep| !std::ptr::eq(*dep, *refinement))
                    .collect();
                assert!(
                    installs_agree(declarations, &rest, contexts, true),
                    "refinement taken for a duplicate: {refinement:?} in {declarations:#?}"
                );
            }
        }
        if let Some(removed) = duplicate_to_remove(declarations) {
            let owned: Vec<DeclaredDependency> =
                declarations.iter().map(|dep| (*dep).clone()).collect();
            assert_eq!(
                detect(&owned)[0].origins.last(),
                Some(&Origin::Manifest(removed.origin.clone())),
                "issue not on the line --fix removes in {declarations:#?}"
            );
        }
        for removed in removable_duplicates(declarations) {
            let rest: Vec<&DeclaredDependency> = declarations
                .iter()
                .copied()
                .filter(|dep| dep.origin != removed.origin)
                .collect();
            assert!(
                installs_agree(declarations, &rest, contexts, false),
                "--fix changes an install: removing {removed:?} from {declarations:#?}"
            );
        }
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        const SPECIFIERS: [&str; 3] = [">=7.0", "<9", "!=8.3.0"];
        const EXTRAS: [&str; 3] = ["auth", "socks", "fast-io"];
        const MARKER: &str = "python_version < '3.11'";

        /// One declaration of `pkg`, independent of how it is spelled.
        #[derive(Debug, Clone)]
        struct Decl {
            context: usize,
            marker: bool,
            specifiers: Vec<usize>,
            extras: Vec<usize>,
            file: usize,
            line: u32,
        }

        fn decl() -> impl Strategy<Value = Decl> {
            (
                0usize..5,
                any::<bool>(),
                prop::sample::subsequence((0..SPECIFIERS.len()).collect::<Vec<_>>(), 0..=3),
                prop::sample::subsequence((0..EXTRAS.len()).collect::<Vec<_>>(), 0..=2),
            )
                .prop_map(|(context, marker, specifiers, extras)| Decl {
                    context,
                    marker,
                    specifiers,
                    extras,
                    file: 0,
                    line: 1,
                })
        }

        /// Declarations drawn from a few requirements, so most share their
        /// requirement with another one; each sits on its own line except
        /// where `reread` repeats an earlier one's origin.
        fn decls() -> impl Strategy<Value = Vec<Decl>> {
            (
                prop::collection::vec(decl(), 1..4),
                prop::collection::vec((any::<prop::sample::Index>(), any::<bool>()), 1..7),
                any::<prop::sample::Index>(),
                any::<bool>(),
            )
                .prop_map(|(bases, picks, reread, rereads)| {
                    let mut out: Vec<Decl> = picks
                        .iter()
                        .zip(1u32..)
                        .map(|((pick, other_context), line)| {
                            let mut decl = pick.get(&bases).clone();
                            if *other_context {
                                decl.context = (decl.context + 1) % 5;
                            }
                            decl.file = usize::from(line % 2 == 0);
                            decl.line = line;
                            decl
                        })
                        .collect();
                    if rereads {
                        let source = reread.get(&out).clone();
                        out.push(source);
                    }
                    out
                })
        }

        /// Spells `decl` out: `spelling` picks the specifier and extra order,
        /// the whitespace around commas and the case and separator of extras,
        /// none of which changes the requirement.
        fn render(decl: &Decl, spelling: u8) -> DeclaredDependency {
            let context = match decl.context {
                0 => DependencyContext::Runtime,
                1 => group("dev"),
                2 => group("lint"),
                3 => extra("x"),
                _ => extra("y"),
            };
            let mut dep = at(
                "pkg",
                context,
                ["pyproject.toml", "setup.cfg"][decl.file],
                decl.line,
            );
            let mut specifiers: Vec<&str> =
                decl.specifiers.iter().map(|&i| SPECIFIERS[i]).collect();
            if spelling & 1 != 0 {
                specifiers.reverse();
            }
            let comma = [",", ", ", " ,  ", "\t,"][usize::from((spelling >> 1) & 3)];
            if !specifiers.is_empty() {
                dep.specifier = Some(specifiers.join(comma));
            }
            let mut extras: Vec<String> = decl
                .extras
                .iter()
                .map(|&i| match (spelling >> 3) & 3 {
                    0 => EXTRAS[i].to_owned(),
                    1 => EXTRAS[i].to_uppercase(),
                    2 => EXTRAS[i].replace('-', "_"),
                    _ => EXTRAS[i].replace('-', "."),
                })
                .collect();
            if spelling & 32 != 0 {
                extras.reverse();
            }
            dep.extras = extras;
            if decl.marker {
                dep.marker = Some(MARKER.to_owned());
            }
            dep
        }

        fn rendered(decls: &[Decl], spellings: &[u8]) -> Vec<DeclaredDependency> {
            decls
                .iter()
                .zip(spellings.iter().cycle())
                .map(|(decl, &spelling)| render(decl, spelling))
                .collect()
        }

        /// What a report says, apart from the origins' order.
        fn summary(found: &[IssueCandidate]) -> Vec<(String, Severity, Confidence, Vec<String>)> {
            found
                .iter()
                .map(|issue| {
                    let mut origins: Vec<String> = issue
                        .origins
                        .iter()
                        .map(|origin| format!("{origin:?}"))
                        .collect();
                    origins.sort();
                    (
                        issue.message.clone(),
                        issue.severity,
                        issue.confidence,
                        origins,
                    )
                })
                .collect()
        }

        proptest! {
            /// Whether two declarations duplicate each other does not depend
            /// on which one is asked about first, so the report never hinges
            /// on declaration order.
            #[test]
            fn duplicates_is_symmetric(
                decls in decls(),
                spellings in prop::collection::vec(any::<u8>(), 1..6),
            ) {
                let deps = rendered(&decls, &spellings);
                let refs: Vec<&DeclaredDependency> = deps.iter().collect();
                for a in &deps {
                    for b in &deps {
                        prop_assert_eq!(duplicates(a, b, &refs), duplicates(b, a, &refs));
                    }
                }
            }

            /// Declarations under different markers, or two reads of one
            /// line, are never duplicates, whatever else they share.
            #[test]
            fn different_marker_or_same_origin_never_duplicates(
                decls in decls(),
                spellings in prop::collection::vec(any::<u8>(), 1..6),
            ) {
                let deps = rendered(&decls, &spellings);
                let refs: Vec<&DeclaredDependency> = deps.iter().collect();
                for a in &deps {
                    for b in &deps {
                        if a.marker != b.marker || a.origin == b.origin {
                            prop_assert!(!duplicates(a, b, &refs), "{a:?} {b:?}");
                        }
                    }
                }
            }

            /// Specifier order and whitespace (`>=7.0,<9` vs `<9, >=7.0`)
            /// and extra order, case and separators (`pkg[A,b]` vs
            /// `pkg[b, a]`) never change what CHK009 reports.
            #[test]
            fn detection_ignores_requirement_spelling(
                decls in decls(),
                first in prop::collection::vec(any::<u8>(), 1..6),
                second in prop::collection::vec(any::<u8>(), 1..6),
            ) {
                prop_assert_eq!(
                    summary(&detect(&rendered(&decls, &first))),
                    summary(&detect(&rendered(&decls, &second)))
                );
            }

            /// The bucketed checks mark exactly the declarations the
            /// pairwise definition does (#1048).
            #[test]
            fn buckets_match_the_pairwise_definition(
                decls in decls(),
                spellings in prop::collection::vec(any::<u8>(), 1..6),
            ) {
                let deps = rendered(&decls, &spellings);
                let refs: Vec<&DeclaredDependency> = deps.iter().collect();
                let prepared = prepare(&refs);
                let repeated = repeated_in_context(&prepared);
                let either: Vec<bool> = repeated
                    .iter()
                    .zip(covered_by_runtime(&prepared))
                    .map(|(repeated, covered)| *repeated || covered)
                    .collect();
                prop_assert_eq!((repeated, either), pairwise(&refs));
            }

            /// Reordering the declarations never changes the report.
            #[test]
            fn detection_ignores_declaration_order(
                decls in decls().prop_shuffle(),
                spelling in any::<u8>(),
            ) {
                let deps = rendered(&decls, &[spelling]);
                let mut reversed = deps.clone();
                reversed.reverse();
                prop_assert_eq!(summary(&detect(&deps)), summary(&detect(&reversed)));
            }
        }
    }
}
