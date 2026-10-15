//! Input the analysis could not read, and the one stage that lowers the
//! findings it may hide (ADR 0005).
//!
//! A gap is recorded where it is found; each kind declares which findings it
//! weakens, so no rule has to know about unread input itself.

use std::collections::BTreeSet;

use crate::config::Confidence;
use crate::graph::ProjectGraph;
use crate::manifest::{LoadedManifest, ManifestWarning};
use crate::parser::ParseSummary;
use crate::path_util::join_rel;
use crate::reachability::ReachabilityReport;
use crate::rules::types::{IssueCandidate, IssueSubject, RuleId, Severity};

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
    /// A reachable notebook was read, but some cells that may be code were
    /// not (#902): what those cells import is unknown.
    UnreadNotebookCells,
    /// A reachable module loads a module whose name is computed at runtime.
    OpaqueDynamicImport,
    /// A manifest declares its runtime dependencies in a form that could not
    /// be read statically (#491): the root's for every finding, a member's
    /// for that member's (#910).
    RuntimeDependenciesUnknown,
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
    /// so it lowers the missing-declaration findings. A script's own block
    /// is read separately, so no project gap reaches its findings.
    const fn lowering(self, candidate: &IssueCandidate) -> Option<Lowering> {
        match (self, candidate.rule, &candidate.subject) {
            (
                Self::UnreadModule | Self::UnreadNotebookCells | Self::OpaqueDynamicImport,
                RuleId::Chk001,
                _,
            )
            | (
                Self::UnreadModule | Self::UnreadNotebookCells,
                RuleId::Chk002,
                IssueSubject::Distribution { .. },
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
            Self::UnreadNotebookCells => {
                "some code cells of a reachable notebook could not be read"
            },
            Self::OpaqueDynamicImport => {
                "a reachable module imports a module by a name computed at runtime"
            },
            Self::RuntimeDependenciesUnknown => "runtime dependencies could not be read statically",
        }
    }
}

/// Gaps that can affect a finding: one in a module that never runs hides
/// nothing, so module gaps count only where the module is reached.
#[must_use]
pub fn collect_gaps(
    parse: &ParseSummary,
    graph: &ProjectGraph,
    reachability: &ReachabilityReport,
    manifest: &LoadedManifest,
    members: &[MemberManifest<'_>],
) -> Vec<Gap> {
    let mut gaps = module_gaps(parse, graph, reachability);
    gaps.extend(manifest_gap(manifest));
    gaps.extend(members.iter().filter_map(|member| {
        let mut gap = manifest_gap(member.manifest)?;
        gap.file = join_rel(member.path, &gap.file);
        gap.member = Some(member.id.to_owned());
        Some(gap)
    }));
    // The root's gap sorts before its members', so it is the one `--explain` names first.
    gaps.sort_by(|left, right| {
        (left.kind, left.member.is_some(), &left.file).cmp(&(
            right.kind,
            right.member.is_some(),
            &right.file,
        ))
    });
    gaps
}

fn module_gaps(
    parse: &ParseSummary,
    graph: &ProjectGraph,
    reachability: &ReachabilityReport,
) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for module in &parse.modules {
        let reached = graph
            .file_id(&module.path)
            .is_some_and(|file_id| reachability.reachable.contains(&file_id));
        if !reached {
            continue;
        }
        let mut push = |kind| {
            gaps.push(Gap {
                file: module.path.clone(),
                kind,
                member: None,
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
    })
}

/// Lower each finding a gap's scope covers and name the gap in `--explain`
/// (ADR 0005 §4). Exit codes still follow severity alone (§7). A cap never
/// reaches `Maybe`, so a severity a rule derived from confidence (a library
/// orphan's) still matches the lowered confidence.
pub(super) fn apply_gaps(candidates: &mut [IssueCandidate], gaps: &[Gap]) {
    for candidate in candidates {
        let in_scope: Vec<&Gap> = gaps
            .iter()
            .filter(|gap| gap.member.is_none() || gap.member == candidate.workspace_member)
            .collect();
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

/// The gap reason, the first file in backticks, and how many more files share the kind.
fn gap_detail(kind: GapKind, gaps: &[&Gap]) -> String {
    let mut files = gaps
        .iter()
        .filter(|gap| gap.kind == kind)
        .map(|gap| &gap.file);
    let first = files.next().map_or("", String::as_str);
    match files.count() {
        0 => format!("{}: `{first}`", kind.reason()),
        more => format!("{}: `{first}` (and {more} more)", kind.reason()),
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
    fn unread_module_caps_absence_findings_but_not_script_ones() {
        let found = lowered(
            &[gap("acme/latin.py", GapKind::UnreadModule)],
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

    #[test]
    fn opaque_dynamic_import_caps_only_unused_files() {
        let found = lowered(
            &[gap("acme/main.py", GapKind::OpaqueDynamicImport)],
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
}
