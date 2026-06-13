//! Apply an [`EntryPlan`] to the project graph.

use crate::graph::{EntryNode, GraphEdge, ProjectGraph};

use super::types::{EntryOrigin, EntryPlan, EntryRoot};

/// Add entry nodes and `Entry reaches File` edges from `plan`.
///
/// # Errors
///
/// Returns [`crate::graph::GraphError`] when graph invariants are violated.
pub fn apply_entry_plan(
    graph: &mut ProjectGraph,
    plan: &EntryPlan,
) -> Result<(), crate::graph::GraphError> {
    for root in &plan.roots {
        let entry_id = graph.intern_entry(EntryNode {
            label: entry_label(root),
            context: root.context,
        });
        if let Some(file_id) = graph.file_id(&root.spec.path) {
            graph.push_edge(GraphEdge::EntryReachesFile {
                entry: entry_id,
                file: file_id,
            });
        }
    }
    Ok(())
}

fn entry_label(root: &EntryRoot) -> String {
    for origin in &root.origins {
        if let Some(label) = origin_label(origin) {
            return label;
        }
    }
    format!("entry:{}", root.spec.path)
}

fn origin_label(origin: &EntryOrigin) -> Option<String> {
    match origin {
        EntryOrigin::Config => None,
        EntryOrigin::Manifest { name, group } => Some(format!("{group}:{name}")),
        EntryOrigin::Plugin { plugin, label } => Some(format!("{}:{label}", plugin.as_key())),
        EntryOrigin::Auto { rule } => Some(format!("auto:{rule}")),
        EntryOrigin::SymbolRef { label, .. } => Some(format!("symbol:{label}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EntrySpec;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::{FileNode, GraphEdge, ProjectGraph};
    use crate::sources::{FileContext, FileKind};

    use super::super::types::{EntryPlan, EntryRoot, ResolvedMode};
    use crate::config::ProjectMode;
    use crate::resolver::ResolveConfidence;

    #[test]
    fn apply_adds_entry_reaches_file_edges() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        });
        graph
            .intern_file(FileNode {
                path: "manage.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("file");

        let plan = EntryPlan {
            mode: ResolvedMode {
                mode: ProjectMode::App,
                confidence: ResolveConfidence::Certain,
            },
            roots: vec![EntryRoot {
                spec: EntrySpec {
                    path: "manage.py".to_owned(),
                    symbol: None,
                },
                context: FileContext::Runtime,
                origins: vec![EntryOrigin::Auto {
                    rule: "manage.py".to_owned(),
                }],
            }],
            warnings: Vec::new(),
        };

        apply_entry_plan(&mut graph, &plan).expect("apply");
        assert_eq!(graph.entry_count(), 1);
        assert!(
            graph
                .edges()
                .iter()
                .any(|edge| matches!(edge, GraphEdge::EntryReachesFile { .. }))
        );
    }
}
