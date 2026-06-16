//! Apply resolution results to the project graph.

use crate::graph::{GraphEdge, GraphError, ModuleOrigin, ProjectGraph};

use super::types::ResolutionIndex;

/// Update module origins and add distribution → module edges from resolution output.
///
/// # Errors
///
/// Returns [`GraphError::Invariant`] when referenced graph nodes are missing.
pub fn apply_resolution_to_graph(
    graph: &mut ProjectGraph,
    index: &ResolutionIndex,
) -> Result<(), GraphError> {
    for resolved in &index.imports {
        let module_id =
            graph
                .module_id(&resolved.full_module)
                .ok_or_else(|| GraphError::Invariant {
                    detail: format!("module `{}` missing from graph", resolved.full_module),
                })?;
        graph.set_module_origin(module_id, resolved.origin);

        if resolved.origin == ModuleOrigin::ThirdParty {
            let Some(distribution_name) = &resolved.distribution else {
                continue;
            };
            let distribution_id = graph.ensure_distribution(distribution_name);
            graph.push_edge(GraphEdge::DistributionProvidesModule {
                distribution: distribution_id,
                module: module_id,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::ModuleOrigin;
    use crate::graph::{FileNode, GraphEdge};
    use crate::parser::ImportContext;
    use crate::resolver::types::{
        ResolutionIndex, ResolveConfidence, ResolvedImport, TransitiveIndex,
    };
    use crate::sources::{FileContext, FileKind};

    #[test]
    fn adds_distribution_provides_module_edge() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        });
        graph
            .intern_file(FileNode {
                path: "app.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("file");
        let _module_id = graph.intern_module("yaml".to_owned(), ModuleOrigin::Unknown);

        let index = ResolutionIndex {
            imports: vec![ResolvedImport {
                import_root: "yaml".to_owned(),
                full_module: "yaml".to_owned(),
                file: "app.py".to_owned(),
                workspace_member: None,
                line: 1,
                context: ImportContext::Runtime,
                optional: false,
                platform_guarded: false,
                origin: ModuleOrigin::ThirdParty,
                distribution: Some("pyyaml".to_owned()),
                confidence: ResolveConfidence::Certain,
            }],
            warnings: Vec::new(),
            transitive: TransitiveIndex::empty(),
            binary_resolutions: BTreeMap::new(),
        };

        apply_resolution_to_graph(&mut graph, &index).expect("apply");
        assert!(
            graph
                .edges()
                .iter()
                .any(|edge| matches!(edge, GraphEdge::DistributionProvidesModule { .. }))
        );
        assert!(graph.distribution_id("pyyaml").is_some());
    }
}
