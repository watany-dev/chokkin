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
    use crate::reachability::types::{ReachPredecessor, ReachabilityReport, TracePath, TraceStep};
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
            reached_opaque_dynamic_import: false,
        };
        assert!(trace_to_file(&report, file_id).is_none());
    }

    #[test]
    fn trace_reconstructs_import_chain() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        });
        let mut intern = |path: &str| {
            graph
                .intern_file(FileNode {
                    path: path.to_owned(),
                    context: FileContext::Runtime,
                    kind: FileKind::Python,
                })
                .expect("intern file")
        };
        let main = intern("main.py");
        let mid = intern("mid.py");
        let child = intern("child.py");

        // Inserted out of path order so the test does not depend on map order.
        let mut predecessors = indexmap::IndexMap::new();
        predecessors.insert(
            child,
            ReachPredecessor {
                from: Some(mid),
                step: TraceStep::Import {
                    module: "child".to_owned(),
                    line: 3,
                },
            },
        );
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
            mid,
            ReachPredecessor {
                from: Some(main),
                step: TraceStep::Import {
                    module: "mid".to_owned(),
                    line: 1,
                },
            },
        );

        let report = ReachabilityReport {
            reachable: IndexSet::from([main, mid, child]),
            unreachable: Vec::new(),
            used_modules: Vec::new(),
            framework_used: IndexSet::new(),
            predecessors,
            reached_opaque_dynamic_import: false,
        };

        let trace = trace_to_file(&report, child).expect("trace");
        assert_eq!(
            trace,
            TracePath {
                target: child,
                steps: vec![
                    TraceStep::File {
                        file: main,
                        path: "main.py".to_owned(),
                    },
                    TraceStep::Import {
                        module: "mid".to_owned(),
                        line: 1,
                    },
                    TraceStep::Import {
                        module: "child".to_owned(),
                        line: 3,
                    },
                ],
            }
        );
    }
}
