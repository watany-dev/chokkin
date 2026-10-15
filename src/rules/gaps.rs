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
use crate::reachability::ReachabilityReport;
use crate::rules::types::{IssueCandidate, IssueSubject, RuleId, Severity};

/// One piece of input the analysis could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    /// Root-relative file the gap is in.
    pub file: String,
    /// What could not be read.
    pub kind: GapKind,
}

/// What could not be read, which decides the findings a gap lowers.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GapKind {
    /// A reachable module that was skipped: none of its imports are known.
    UnreadModule,
    /// A reachable module loads a module whose name is computed at runtime.
    OpaqueDynamicImport,
    /// The root manifest declares its runtime dependencies in a form that
    /// could not be read statically (#491).
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
            (Self::UnreadModule | Self::OpaqueDynamicImport, RuleId::Chk001, _)
            | (Self::UnreadModule, RuleId::Chk002, IssueSubject::Distribution { .. }) => {
                Some(Lowering::Cap(Confidence::Likely))
            },
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
) -> Vec<Gap> {
    let mut gaps = module_gaps(parse, graph, reachability);
    gaps.extend(manifest_gap(manifest));
    gaps.sort_by(|left, right| (left.kind, &left.file).cmp(&(right.kind, &right.file)));
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
            });
        };
        if module.skipped {
            push(GapKind::UnreadModule);
        }
        if module.has_opaque_dynamic_import {
            push(GapKind::OpaqueDynamicImport);
        }
    }
    gaps
}

/// The manifest's warning names the file; `dynamic = ["dependencies"]` is the
/// only flag without one, and it lives in `pyproject.toml`.
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
    })
}

/// Lower each finding a gap's scope covers and name the gap in `--explain`
/// (ADR 0005 §5). Exit codes still follow severity alone (§7).
pub(super) fn apply_gaps(candidates: &mut [IssueCandidate], gaps: &[Gap]) {
    let kinds: BTreeSet<GapKind> = gaps.iter().map(|gap| gap.kind).collect();
    for candidate in candidates {
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
            candidate.explain.details.push(gap_detail(kind, gaps));
        }
    }
}

/// The gap reason, the first file in backticks, and how many more files share the kind.
fn gap_detail(kind: GapKind, gaps: &[Gap]) -> String {
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
        }
    }

    fn lowered(gaps: &[Gap], candidates: Vec<IssueCandidate>) -> Vec<(Severity, Confidence)> {
        let mut candidates = candidates;
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

    #[test]
    fn a_cap_never_raises_confidence() {
        let mut maybe = candidate(RuleId::Chk001, file("acme/orphan.py"));
        maybe.confidence = Confidence::Maybe;
        let found = lowered(&[gap("acme/latin.py", GapKind::UnreadModule)], vec![maybe]);
        assert_eq!(found, [(Severity::Error, Confidence::Maybe)]);
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
