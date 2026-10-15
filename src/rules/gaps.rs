//! Input the analysis could not read, and the one stage that lowers the
//! findings it may hide (ADR 0005).
//!
//! A gap is recorded where it is found; each kind declares which findings it
//! weakens, so no rule has to know about unread input itself.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::Confidence;
use crate::entry::resolve_module_to_path;
use crate::graph::ProjectGraph;
use crate::manifest::{InlineScript, LoadedManifest, ManifestWarning};
use crate::parser::{ImportContext, ParseSummary};
use crate::path_util::join_rel;
use crate::plugins::{CommandTarget, CommandTargetRef, PluginHints, UnreadSettings};
use crate::reachability::ReachabilityReport;
use crate::resolver::{ResolveWarning, is_first_party_import};
use crate::rules::RuleContext;
use crate::rules::deps::file_context;
use crate::rules::types::{IssueCandidate, IssueSubject, RuleId, Severity, import_site};
use crate::sources::FileContext;

/// One piece of input the analysis could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    /// Root-relative file the gap is in.
    pub file: String,
    /// What could not be read.
    pub kind: GapKind,
    /// The workspace member whose findings alone the gap lowers; `None`
    /// lowers every finding in its kind's scope (#910).
    pub member: Option<String>,
    /// What in the file could not be read and its line, which `--explain`
    /// shows in place of the file: an unresolved import's module (#911), a
    /// command's target (#913).
    pub site: Option<(String, Option<u32>)>,
}

/// A workspace member's manifest: what it could not read lowers only the
/// findings in that member.
#[derive(Debug, Clone, Copy)]
pub struct MemberManifest<'a> {
    /// The member id findings carry in `workspace_member`.
    pub id: &'a str,
    /// Root-relative member directory, which the manifest's files are under.
    pub path: &'a str,
    /// The member's own manifest.
    pub manifest: &'a LoadedManifest,
}

/// What could not be read, which decides the findings a gap lowers.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GapKind {
    /// A reachable module that was skipped: none of its imports are known.
    UnreadModule,
    /// A skipped public module of a library that no root reaches: a caller
    /// may import it, so it may use any declared distribution (#550). It
    /// leaves orphans alone, since what it imports must ship in the wheel
    /// and so is public too.
    UnreadPublicModule,
    /// A reachable notebook was read, but some cells that may be code were
    /// not (#902): what those cells import is unknown.
    UnreadNotebookCells,
    /// A reachable module loads a module whose name is computed at runtime.
    OpaqueDynamicImport,
    /// A manifest declares its runtime dependencies in a form that could not
    /// be read statically (#491): the root's for every finding, a member's
    /// for that member's (#910).
    RuntimeDependenciesUnknown,
    /// A reachable import no source resolved to a module (§7): any declared
    /// distribution may provide it (#911).
    UnresolvedImport,
    /// The same in a PEP 723 script, whose own block installs what it
    /// imports: it lowers that script's findings alone (#911).
    UnresolvedScriptImport,
    /// A task file or tool config runs Python code no entry reaches: a
    /// computed or unread target, or a file or module it names that nothing
    /// else reaches (#913). What that code imports is unknown.
    UnreadCommandTarget,
    /// A Django project's settings could not be found or read, or star-import
    /// a module that is not a project file (#914): the apps, middleware and
    /// URL configuration they name are unknown.
    UnreadDjangoSettings,
}

/// How a gap changes a finding in its scope.
enum Lowering {
    /// Confidence falls to at most this level.
    Cap(Confidence),
    /// The finding becomes an info hint at `Maybe`.
    Hint,
}

impl GapKind {
    /// The scope each kind declares: what it lowers and how, if at all.
    ///
    /// Unread code may import any file or distribution, so it lowers the
    /// absence findings; an unknown declaration list may declare any import,
    /// so it lowers the missing-declaration findings; an unresolved import may
    /// name what an unused declaration provides. A script's own block is read
    /// separately, so no project gap reaches its findings.
    const fn lowering(self, candidate: &IssueCandidate) -> Option<Lowering> {
        match (self, candidate.rule, &candidate.subject) {
            (
                Self::UnreadModule
                | Self::UnreadNotebookCells
                | Self::OpaqueDynamicImport
                | Self::UnreadCommandTarget
                | Self::UnreadDjangoSettings,
                RuleId::Chk001,
                _,
            )
            | (
                Self::UnreadModule
                | Self::UnreadPublicModule
                | Self::UnreadNotebookCells
                | Self::UnreadDjangoSettings,
                RuleId::Chk002,
                IssueSubject::Distribution { .. },
            )
            | (Self::UnresolvedImport, RuleId::Chk002, IssueSubject::Distribution { .. })
            | (
                Self::UnresolvedScriptImport,
                RuleId::Chk002,
                IssueSubject::ScriptDistribution { .. },
            ) => Some(Lowering::Cap(Confidence::Likely)),
            (
                Self::RuntimeDependenciesUnknown,
                RuleId::Chk003 | RuleId::Chk004,
                IssueSubject::Import { .. },
            ) => Some(Lowering::Hint),
            _ => None,
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::UnreadModule => "a reachable module could not be read",
            Self::UnreadPublicModule => "a public module of the library could not be read",
            Self::UnreadNotebookCells => {
                "some code cells of a reachable notebook could not be read"
            },
            Self::OpaqueDynamicImport => {
                "a reachable module imports a module by a name computed at runtime"
            },
            Self::RuntimeDependenciesUnknown => "runtime dependencies could not be read statically",
            Self::UnresolvedImport => "a reachable import could not be resolved to a distribution",
            Self::UnresolvedScriptImport => {
                "an import in the script could not be resolved to a distribution"
            },
            Self::UnreadCommandTarget => "a command runs Python code no entry reaches",
            Self::UnreadDjangoSettings => "the Django settings could not be found or read",
        }
    }
}

impl Gap {
    /// Whether the gap is in `candidate`'s scope: a member's gap reaches that
    /// member's findings, a script's import that script's alone.
    fn reaches(&self, candidate: &IssueCandidate) -> bool {
        let in_member = self.member.is_none() || self.member == candidate.workspace_member;
        let in_script = match (self.kind, &candidate.subject) {
            (GapKind::UnresolvedScriptImport, IssueSubject::ScriptDistribution { script, .. }) => {
                *script == self.file
            },
            _ => true,
        };
        in_member && in_script
    }

    /// What `--explain` names: the import or target and its line, or else the file.
    fn label(&self) -> String {
        match &self.site {
            Some((what, line)) => format!("`{what}` in `{}`", import_site(&self.file, *line)),
            None => format!("`{}`", self.file),
        }
    }
}

/// Gaps that can affect a finding.
///
/// One in a module that never runs hides nothing, so module and import gaps
/// count only where the module is reached or, for an unread module, where a
/// caller of the library may import it.
#[must_use]
pub fn collect_gaps(
    context: &RuleContext<'_>,
    manifest: &LoadedManifest,
    members: &[MemberManifest<'_>],
    scripts: &[InlineScript],
    plugins: &PluginHints,
) -> Vec<Gap> {
    let mut gaps = module_gaps(context.parse, context.graph, context.reachability);
    gaps.extend(unread_public_module_gaps(context.reachability));
    gaps.extend(unresolved_import_gaps(context, manifest, scripts));
    gaps.extend(command_target_gaps(
        context,
        &plugins.config_command_targets,
    ));
    gaps.extend(plugins.unread_settings().map(settings_gap));
    gaps.extend(manifest_gap(manifest));
    gaps.extend(members.iter().filter_map(|member| {
        let mut gap = manifest_gap(member.manifest)?;
        gap.file = join_rel(member.path, &gap.file);
        gap.member = Some(member.id.to_owned());
        Some(gap)
    }));
    // The root's gap sorts before its members', so it is the one `--explain` names first.
    gaps.sort_by(|left, right| {
        (left.kind, left.member.is_some(), &left.file, &left.site).cmp(&(
            right.kind,
            right.member.is_some(),
            &right.file,
            &right.site,
        ))
    });
    gaps
}

fn is_reached(file: &str, graph: &ProjectGraph, reachability: &ReachabilityReport) -> bool {
    graph
        .file_id(file)
        .is_some_and(|file_id| reachability.reachable.contains(&file_id))
}

fn module_gaps(
    parse: &ParseSummary,
    graph: &ProjectGraph,
    reachability: &ReachabilityReport,
) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for module in &parse.modules {
        if !is_reached(&module.path, graph, reachability) {
            continue;
        }
        let mut push = |kind| {
            gaps.push(Gap {
                file: module.path.clone(),
                kind,
                member: None,
                site: None,
            });
        };
        if module.skipped {
            push(GapKind::UnreadModule);
        }
        if module.has_unread_cells {
            push(GapKind::UnreadNotebookCells);
        }
        if module.has_opaque_dynamic_import {
            push(GapKind::OpaqueDynamicImport);
        }
    }
    gaps
}

/// Skipped modules no root reaches that a caller of the library may import:
/// CHK002 counts the imports of the public orphans it could read (#501), so
/// it must allow for the ones it could not.
fn unread_public_module_gaps(reachability: &ReachabilityReport) -> Vec<Gap> {
    reachability
        .unread
        .iter()
        .filter(|file| file.callers_may_import())
        .map(|file| Gap {
            file: file.path.clone(),
            kind: GapKind::UnreadPublicModule,
            member: None,
            site: None,
        })
        .collect()
}

/// One gap per file and module, at the import sites CHK010 warns about: a
/// `TYPE_CHECKING` or optional import never fails, a docs or dev file runs
/// outside the installed package, and a first-party miss is a typo rather
/// than a distribution (§12). A script's block installs what the script runs
/// with, so its imports count in any context.
fn unresolved_import_gaps(
    context: &RuleContext<'_>,
    manifest: &LoadedManifest,
    scripts: &[InlineScript],
) -> Vec<Gap> {
    let is_script = |file: &str| scripts.iter().any(|script| script.path == file);
    let mut sites: BTreeMap<(&str, &str), Option<u32>> = BTreeMap::new();
    for warning in &context.resolution.warnings {
        let ResolveWarning::UnresolvedImport {
            import,
            file,
            line,
            context: import_context,
            optional,
            ..
        } = warning
        else {
            continue;
        };
        let counts = *import_context != ImportContext::Type
            && !*optional
            && is_reached(file, context.graph, context.reachability)
            && (is_script(file)
                || !matches!(
                    file_context(file, context.sources),
                    FileContext::Docs | FileContext::Dev
                ))
            && !is_first_party_import(import, &context.sources.layout, &manifest.metadata);
        if !counts {
            continue;
        }
        keep_first_line(&mut sites, (file.as_str(), import.as_str()), *line);
    }
    sites
        .into_iter()
        .map(|((file, import), line)| Gap {
            file: file.to_owned(),
            kind: if is_script(file) {
                GapKind::UnresolvedScriptImport
            } else {
                GapKind::UnresolvedImport
            },
            member: None,
            site: Some((import.to_owned(), line)),
        })
        .collect()
}

/// Keep the first line of a site, as CHK010 anchors at it; a site without
/// one sorts last.
fn keep_first_line<K: Ord>(sites: &mut BTreeMap<K, Option<u32>>, key: K, line: Option<u32>) {
    let kept = sites.entry(key).or_insert(line);
    *kept = match (*kept, line) {
        (Some(kept), Some(line)) => Some(kept.min(line)),
        (kept, line) => kept.or(line),
    };
}

/// One gap per command file and target. A computed or unread target may run
/// any file; a file or module it names that no entry reaches runs code whose
/// imports are not followed, since commands add no entries yet (#912). A
/// target that names nothing discovered (a typo, a deleted file, a
/// third-party module such as `pytest`) hides no project file, so it is no gap.
fn command_target_gaps(context: &RuleContext<'_>, commands: &[CommandTargetRef]) -> Vec<Gap> {
    let known_paths: BTreeSet<String> = context
        .sources
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    let unreached = |path: &String| !is_reached(path, context.graph, context.reachability);
    let mut sites: BTreeMap<(&str, String), Option<u32>> = BTreeMap::new();
    for command in commands {
        let text = match &command.target {
            CommandTarget::Opaque { text } => text.clone(),
            CommandTarget::Script { paths } => {
                match paths.iter().find(|path| known_paths.contains(*path)) {
                    Some(path) if unreached(path) => path.clone(),
                    _ => continue,
                }
            },
            CommandTarget::Module { module } => {
                let layout = &context.sources.layout;
                let path =
                    resolve_module_to_path(&format!("{module}.__main__"), layout, &known_paths)
                        .or_else(|| resolve_module_to_path(module, layout, &known_paths));
                match path {
                    Some(path) if unreached(&path) => module.clone(),
                    _ => continue,
                }
            },
        };
        keep_first_line(
            &mut sites,
            (command.origin.file.as_str(), text),
            command.origin.line,
        );
    }
    sites
        .into_iter()
        .map(|((file, text), line)| Gap {
            file: file.to_owned(),
            kind: GapKind::UnreadCommandTarget,
            member: None,
            site: Some((text, line)),
        })
        .collect()
}

/// Settings decide what Django runs in every member, so the gap lowers the
/// whole project's findings.
fn settings_gap(unread: &UnreadSettings) -> Gap {
    Gap {
        file: unread.file.clone(),
        kind: GapKind::UnreadDjangoSettings,
        member: None,
        site: unread.site.clone(),
    }
}

/// The manifest's warning names the file (`pyproject.toml` for
/// `dynamic = ["dependencies"]`); the fallback keeps the gap if one is missing.
fn manifest_gap(manifest: &LoadedManifest) -> Option<Gap> {
    if !manifest.sources.runtime_dependencies_unknown {
        return None;
    }
    let file = manifest
        .warnings
        .iter()
        .find_map(|warning| match warning {
            ManifestWarning::RuntimeDependenciesUnknown { file } => Some(file.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "pyproject.toml".to_owned());
    Some(Gap {
        file,
        kind: GapKind::RuntimeDependenciesUnknown,
        member: None,
        site: None,
    })
}

/// Lower each finding a gap's scope covers and name the gap in `--explain`
/// (ADR 0005 §4). Exit codes still follow severity alone (§7). A cap never
/// reaches `Maybe`, so a severity a rule derived from confidence (a library
/// orphan's) still matches the lowered confidence.
pub(super) fn apply_gaps(candidates: &mut [IssueCandidate], gaps: &[Gap]) {
    for candidate in candidates {
        let in_scope: Vec<&Gap> = gaps.iter().filter(|gap| gap.reaches(candidate)).collect();
        let kinds: BTreeSet<GapKind> = in_scope.iter().map(|gap| gap.kind).collect();
        for &kind in &kinds {
            let Some(lowering) = kind.lowering(candidate) else {
                continue;
            };
            match lowering {
                Lowering::Cap(cap) => candidate.confidence = candidate.confidence.min(cap),
                Lowering::Hint => {
                    candidate.severity = Severity::Info;
                    candidate.confidence = Confidence::Maybe;
                },
            }
            candidate.explain.details.push(gap_detail(kind, &in_scope));
        }
    }
}

/// The gap reason, the first gap's label, and how many more gaps share the kind.
fn gap_detail(kind: GapKind, gaps: &[&Gap]) -> String {
    let mut labels = gaps
        .iter()
        .filter(|gap| gap.kind == kind)
        .map(|gap| gap.label());
    let first = labels.next().unwrap_or_default();
    match labels.count() {
        0 => format!("{}: {first}", kind.reason()),
        more => format!("{}: {first} (and {more} more)", kind.reason()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::types::ExplainData;

    fn candidate(rule: RuleId, subject: IssueSubject) -> IssueCandidate {
        IssueCandidate {
            rule,
            subject,
            severity: Severity::Error,
            confidence: Confidence::Certain,
            message: String::new(),
            workspace_member: None,
            origins: Vec::new(),
            explain: ExplainData::default(),
        }
    }

    fn file(path: &str) -> IssueSubject {
        IssueSubject::File {
            path: path.to_owned(),
        }
    }

    fn distribution(name: &str) -> IssueSubject {
        IssueSubject::Distribution {
            name: name.to_owned(),
        }
    }

    fn script_distribution(name: &str) -> IssueSubject {
        IssueSubject::ScriptDistribution {
            script: "tool.py".to_owned(),
            name: name.to_owned(),
        }
    }

    fn import(module: &str) -> IssueSubject {
        IssueSubject::Import {
            module: module.to_owned(),
            file: "acme/main.py".to_owned(),
            line: Some(1),
            distribution: None,
        }
    }

    fn gap(file: &str, kind: GapKind) -> Gap {
        Gap {
            file: file.to_owned(),
            kind,
            member: None,
            site: None,
        }
    }

    fn in_member(mut candidate: IssueCandidate, member: &str) -> IssueCandidate {
        candidate.workspace_member = Some(member.to_owned());
        candidate
    }

    fn lowered(gaps: &[Gap], mut candidates: Vec<IssueCandidate>) -> Vec<(Severity, Confidence)> {
        apply_gaps(&mut candidates, gaps);
        candidates
            .iter()
            .map(|candidate| (candidate.severity, candidate.confidence))
            .collect()
    }

    #[test]
    fn unread_module_and_django_settings_cap_absence_findings_but_not_script_ones() {
        for gap in [
            gap("acme/latin.py", GapKind::UnreadModule),
            gap("manage.py", GapKind::UnreadDjangoSettings),
        ] {
            let found = lowered(
                &[gap],
                vec![
                    candidate(RuleId::Chk001, file("acme/orphan.py")),
                    candidate(RuleId::Chk002, distribution("requests")),
                    candidate(RuleId::Chk002, script_distribution("rich")),
                    candidate(RuleId::Chk003, import("yaml")),
                ],
            );
            assert_eq!(
                found,
                [
                    (Severity::Error, Confidence::Likely),
                    (Severity::Error, Confidence::Likely),
                    (Severity::Error, Confidence::Certain),
                    (Severity::Error, Confidence::Certain),
                ]
            );
        }
    }

    #[test]
    fn opaque_dynamic_import_and_command_targets_cap_only_unused_files() {
        for gap in [
            gap("acme/main.py", GapKind::OpaqueDynamicImport),
            gap("Makefile", GapKind::UnreadCommandTarget),
        ] {
            let found = lowered(
                &[gap],
                vec![
                    candidate(RuleId::Chk001, file("acme/orphan.py")),
                    candidate(RuleId::Chk002, distribution("requests")),
                ],
            );
            assert_eq!(
                found,
                [
                    (Severity::Error, Confidence::Likely),
                    (Severity::Error, Confidence::Certain),
                ]
            );
        }
    }

    #[test]
    fn unknown_runtime_dependencies_turn_missing_declarations_into_hints() {
        let found = lowered(
            &[gap("setup.py", GapKind::RuntimeDependenciesUnknown)],
            vec![
                candidate(RuleId::Chk003, import("yaml")),
                candidate(RuleId::Chk004, import("idna")),
                candidate(RuleId::Chk003, script_distribution("rich")),
                candidate(RuleId::Chk001, file("acme/orphan.py")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Info, Confidence::Maybe),
                (Severity::Info, Confidence::Maybe),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    /// A member's unread declarations may declare its own imports only, so
    /// they lower that member's findings and leave the root's and other
    /// members' alone (#910).
    #[test]
    fn a_member_gap_lowers_only_that_members_missing_declarations() {
        let mut member_gap = gap(
            "packages/x/pyproject.toml",
            GapKind::RuntimeDependenciesUnknown,
        );
        member_gap.member = Some("x".to_owned());
        let mut candidates = vec![
            in_member(candidate(RuleId::Chk003, import("yaml")), "x"),
            in_member(candidate(RuleId::Chk004, import("idna")), "x"),
            in_member(candidate(RuleId::Chk003, import("yaml")), "y"),
            candidate(RuleId::Chk003, import("yaml")),
        ];
        apply_gaps(&mut candidates, &[member_gap]);
        let found: Vec<_> = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.severity,
                    candidate.confidence,
                    candidate.explain.details.clone(),
                )
            })
            .collect();
        let detail = vec![
            "runtime dependencies could not be read statically: `packages/x/pyproject.toml`"
                .to_owned(),
        ];
        assert_eq!(
            found,
            [
                (Severity::Info, Confidence::Maybe, detail.clone()),
                (Severity::Info, Confidence::Maybe, detail),
                (Severity::Error, Confidence::Certain, Vec::new()),
                (Severity::Error, Confidence::Certain, Vec::new()),
            ]
        );
    }

    /// The root's unread declarations still lower every finding, a member's
    /// included, and `--explain` counts only the gaps in a finding's scope.
    #[test]
    fn the_root_gap_reaches_members_and_explain_counts_gaps_in_scope() {
        let mut member_x = gap("packages/x/setup.py", GapKind::RuntimeDependenciesUnknown);
        member_x.member = Some("x".to_owned());
        let mut member_y = gap("packages/y/setup.py", GapKind::RuntimeDependenciesUnknown);
        member_y.member = Some("y".to_owned());
        let mut candidates = vec![
            in_member(candidate(RuleId::Chk003, import("yaml")), "x"),
            candidate(RuleId::Chk003, import("yaml")),
        ];
        apply_gaps(
            &mut candidates,
            &[
                gap("pyproject.toml", GapKind::RuntimeDependenciesUnknown),
                member_x,
                member_y,
            ],
        );
        assert_eq!(
            candidates[0].explain.details,
            ["runtime dependencies could not be read statically: `pyproject.toml` (and 1 more)"]
        );
        assert_eq!(
            candidates[1].explain.details,
            ["runtime dependencies could not be read statically: `pyproject.toml`"]
        );
        assert_eq!(candidates[1].severity, Severity::Info);
    }

    fn unresolved(file: &str, kind: GapKind, module: &str, line: u32) -> Gap {
        let mut gap = gap(file, kind);
        gap.site = Some((module.to_owned(), Some(line)));
        gap
    }

    /// An unresolved import may name what an unused distribution provides;
    /// files and missing declarations are read from imports that did resolve.
    #[test]
    fn unresolved_import_caps_only_unused_project_distributions() {
        let mut candidates = vec![
            candidate(RuleId::Chk002, distribution("acme-widgets-sdk")),
            candidate(RuleId::Chk002, script_distribution("rich")),
            candidate(RuleId::Chk001, file("acme/orphan.py")),
            candidate(RuleId::Chk003, import("yaml")),
        ];
        apply_gaps(
            &mut candidates,
            &[
                unresolved(
                    "acme/main.py",
                    GapKind::UnresolvedImport,
                    "zz_widgets_core",
                    1,
                ),
                unresolved("acme/cli.py", GapKind::UnresolvedImport, "zz_cli", 3),
            ],
        );
        let found: Vec<_> = candidates
            .iter()
            .map(|candidate| (candidate.confidence, candidate.explain.details.len()))
            .collect();
        assert_eq!(
            found,
            [
                (Confidence::Likely, 1),
                (Confidence::Certain, 0),
                (Confidence::Certain, 0),
                (Confidence::Certain, 0),
            ]
        );
        assert_eq!(
            candidates[0].explain.details,
            [
                "a reachable import could not be resolved to a distribution: `zz_widgets_core` in `acme/main.py:1` (and 1 more)"
            ]
        );
    }

    /// A script's block installs what the script imports, so its unresolved
    /// import lowers that block's entries and no other script's or the project's.
    #[test]
    fn unresolved_script_import_caps_only_its_own_block() {
        let mut other = script_distribution("httpx");
        if let IssueSubject::ScriptDistribution { script, .. } = &mut other {
            "other.py".clone_into(script);
        }
        let found = lowered(
            &[unresolved(
                "tool.py",
                GapKind::UnresolvedScriptImport,
                "zz_render",
                4,
            )],
            vec![
                candidate(RuleId::Chk002, script_distribution("rich")),
                candidate(RuleId::Chk002, other),
                candidate(RuleId::Chk002, distribution("requests")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    #[test]
    fn a_cap_never_raises_confidence() {
        let mut maybe = candidate(RuleId::Chk001, file("acme/orphan.py"));
        maybe.confidence = Confidence::Maybe;
        let found = lowered(&[gap("acme/latin.py", GapKind::UnreadModule)], vec![maybe]);
        assert_eq!(found, [(Severity::Error, Confidence::Maybe)]);
    }

    /// The cells that were read still count, so only absence findings fall;
    /// a missing declaration seen in a read cell is still certain (#902).
    #[test]
    fn unread_notebook_cells_cap_absence_findings_like_an_unread_module() {
        let found = lowered(
            &[gap("acme/demo.ipynb", GapKind::UnreadNotebookCells)],
            vec![
                candidate(RuleId::Chk001, file("acme/orphan.py")),
                candidate(RuleId::Chk002, distribution("requests")),
                candidate(RuleId::Chk002, script_distribution("rich")),
                candidate(RuleId::Chk003, import("yaml")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    /// Module gaps of `modules` (path, skipped, opaque) when `reached` run.
    fn reached_gaps(modules: &[(&str, bool, bool)], reached: &[&str]) -> Vec<(String, GapKind)> {
        use crate::discovery::{ProjectRoot, RootMarker};
        use crate::graph::FileNode;
        use crate::parser::ParsedModule;
        use crate::sources::{FileContext, FileKind};

        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        });
        let mut reachability = ReachabilityReport::default();
        for (path, ..) in modules {
            let file_id = graph
                .intern_file(FileNode {
                    path: (*path).to_owned(),
                    context: FileContext::Runtime,
                    kind: FileKind::Python,
                })
                .expect("file");
            if reached.contains(path) {
                reachability.reachable.insert(file_id);
            }
        }
        let parse = ParseSummary {
            modules: modules
                .iter()
                .map(|&(path, skipped, opaque)| ParsedModule {
                    path: path.to_owned(),
                    skipped,
                    has_opaque_dynamic_import: opaque,
                    ..ParsedModule::default()
                })
                .collect(),
        };
        module_gaps(&parse, &graph, &reachability)
            .into_iter()
            .map(|gap| (gap.file, gap.kind))
            .collect()
    }

    #[test]
    fn only_reached_modules_leave_gaps() {
        let found = reached_gaps(
            &[
                ("acme/main.py", false, true),
                ("acme/latin.py", true, false),
                ("acme/orphan.py", false, true),
                ("acme/stray.py", true, false),
            ],
            &["acme/main.py", "acme/latin.py"],
        );
        assert_eq!(
            found,
            [
                ("acme/main.py".to_owned(), GapKind::OpaqueDynamicImport),
                ("acme/latin.py".to_owned(), GapKind::UnreadModule),
            ]
        );
    }

    /// A library caller may import a public module no root reaches, so an
    /// unread one may use any declared distribution; what it imports ships in
    /// the wheel and is public too, so orphans stay as they are (#550).
    #[test]
    fn an_unread_public_module_leaves_a_gap_that_caps_only_unused_distributions() {
        use crate::config::ProjectMode;
        use crate::graph::FileId;
        use crate::reachability::UnreachableFile;

        let unread = |path: &str, max_confidence| UnreachableFile {
            file: FileId(0),
            path: path.to_owned(),
            max_confidence,
            mode: ProjectMode::Library,
        };
        let mut reachability = ReachabilityReport::default();
        reachability.unread = vec![
            unread("acme/client.py", Confidence::Maybe),
            unread("tools/release.py", Confidence::Certain),
        ];
        let gaps = unread_public_module_gaps(&reachability);
        assert_eq!(gaps, [gap("acme/client.py", GapKind::UnreadPublicModule)]);

        let found = lowered(
            &gaps,
            vec![
                candidate(RuleId::Chk001, file("acme/orphan.py")),
                candidate(RuleId::Chk002, distribution("requests")),
                candidate(RuleId::Chk002, script_distribution("rich")),
                candidate(RuleId::Chk003, import("yaml")),
            ],
        );
        assert_eq!(
            found,
            [
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Likely),
                (Severity::Error, Confidence::Certain),
                (Severity::Error, Confidence::Certain),
            ]
        );
    }

    #[test]
    fn explain_names_each_gap_kind_once() {
        let mut candidates = vec![candidate(RuleId::Chk001, file("acme/orphan.py"))];
        apply_gaps(
            &mut candidates,
            &[
                gap("acme/a.py", GapKind::UnreadModule),
                gap("acme/b.py", GapKind::UnreadModule),
                gap("acme/main.py", GapKind::OpaqueDynamicImport),
            ],
        );
        assert_eq!(
            candidates[0].explain.details,
            [
                "a reachable module could not be read: `acme/a.py` (and 1 more)",
                "a reachable module imports a module by a name computed at runtime: `acme/main.py`",
            ]
        );
    }

    #[test]
    fn keep_first_line_keeps_the_earliest_line_and_prefers_one_to_none() {
        let mut sites = BTreeMap::new();
        for (key, line) in [
            ("later", Some(5)),
            ("later", Some(3)),
            ("earlier", Some(3)),
            ("earlier", Some(5)),
            ("none_first", None),
            ("none_first", Some(4)),
            ("none_last", Some(4)),
            ("none_last", None),
            ("only_none", None),
        ] {
            keep_first_line(&mut sites, key, line);
        }
        assert_eq!(
            sites.into_iter().collect::<Vec<_>>(),
            [
                ("earlier", Some(3)),
                ("later", Some(3)),
                ("none_first", Some(4)),
                ("none_last", Some(4)),
                ("only_none", None),
            ]
        );
    }
}
