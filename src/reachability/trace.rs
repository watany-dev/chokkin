//! Reconstruct reachability trace paths.

use crate::graph::FileId;

use super::types::{ReachabilityReport, TracePath};

/// Reconstruct the shortest known path from an entry root to `target`.
#[must_use]
pub fn trace_to_file(report: &ReachabilityReport, target: FileId) -> Option<TracePath> {
    if !report.reachable.contains(&target) {
        return None;
    }

    let mut steps = Vec::new();
    let mut current = target;

    while let Some(predecessor) = report.predecessors.get(&current) {
        steps.push(predecessor.step.clone());
        let Some(parent) = predecessor.from else {
            break;
        };
        current = parent;
    }
    steps.reverse();

    Some(TracePath { target, steps })
}

#[cfg(test)]
mod tests {
    use indexmap::IndexSet;

    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::{FileId, FileNode, ProjectGraph};
    use crate::reachability::types::{ReachPredecessor, ReachabilityReport, TraceStep};
    use crate::sources::{FileContext, FileKind};

    #[test]
    fn trace_returns_none_for_unreachable_file() {
        let file_id = FileId(0);
        let report = ReachabilityReport {
            reachable: IndexSet::new(),
            unreachable: Vec::new(),
            used_modules: Vec::new(),
            framework_used: IndexSet::new(),
            predecessors: indexmap::IndexMap::new(),
        };
        assert!(trace_to_file(&report, file_id).is_none());
    }

    #[test]
    fn trace_reconstructs_import_chain() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        });
        let main = graph
            .intern_file(FileNode {
                path: "main.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("main");
        let child = graph
            .intern_file(FileNode {
                path: "child.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("child");

        let mut predecessors = indexmap::IndexMap::new();
        predecessors.insert(
            main,
            ReachPredecessor {
                from: None,
                step: TraceStep::File {
                    file: main,
                    path: "main.py".to_owned(),
                },
            },
        );
        predecessors.insert(
            child,
            ReachPredecessor {
                from: Some(main),
                step: TraceStep::Import {
                    module: "child".to_owned(),
                    line: 1,
                },
            },
        );

        let mut reachable = IndexSet::new();
        reachable.insert(main);
        reachable.insert(child);
        let report = ReachabilityReport {
            reachable,
            unreachable: Vec::new(),
            used_modules: Vec::new(),
            framework_used: IndexSet::new(),
            predecessors,
        };

        let trace = trace_to_file(&report, child).expect("trace");
        assert_eq!(trace.target, child);
        assert!(trace.steps.len() >= 2);
    }
}
