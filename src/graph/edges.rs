//! Attach parsed import edges to the project graph.

use crate::parser::ParsedModule;

use super::error::GraphError;
use super::types::{GraphEdge, ModuleOrigin, ProjectGraph};

/// Attach import edges from a parsed module onto the graph.
///
/// # Errors
///
/// Returns [`GraphError::Invariant`] when `file_id` is not registered.
pub fn add_parsed_imports(
    graph: &mut ProjectGraph,
    file_id: super::types::FileId,
    parsed: &ParsedModule,
) -> Result<(), GraphError> {
    if !graph.file_id(&parsed.path).is_some_and(|id| id == file_id) {
        return Err(GraphError::Invariant {
            detail: format!("file id does not match parsed path `{}`", parsed.path),
        });
    }

    push_module_import_edges(
        graph,
        file_id,
        parsed.imports.iter().map(|i| (i.module.as_str(), i.line)),
    );
    push_module_import_edges(
        graph,
        file_id,
        parsed
            .dynamic_imports
            .iter()
            .map(|i| (i.module.as_str(), i.line)),
    );

    Ok(())
}

fn push_module_import_edges<'a>(
    graph: &mut ProjectGraph,
    file_id: super::types::FileId,
    imports: impl IntoIterator<Item = (&'a str, u32)>,
) {
    for (module, line) in imports {
        if module.is_empty() {
            continue;
        }
        let module_id = graph.intern_module(module.to_owned(), ModuleOrigin::Unknown);
        graph.push_edge(GraphEdge::FileImportsModule {
            file: file_id,
            module: module_id,
            line,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::parser::{ImportContext, ImportKind, ImportRef, ParsedModule};
    use crate::sources::{FileContext, FileKind};

    #[test]
    fn adds_import_edge() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        });
        let file_id = graph
            .intern_file(super::super::types::FileNode {
                path: "app.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("file");
        let parsed = ParsedModule {
            path: "app.py".to_owned(),
            imports: vec![ImportRef {
                module: "os".to_owned(),
                name: None,
                alias: None,
                line: 1,
                kind: ImportKind::Import,
                context: ImportContext::Runtime,
                optional: false,
                platform_guarded: false,
                relative_level: 0,
            }],
            dynamic_imports: Vec::new(),
            symbols: Vec::new(),
            exports: Vec::new(),
            ignores: Vec::new(),
            has_opaque_dynamic_import: false,
            diagnostics: Vec::new(),
        };
        add_parsed_imports(&mut graph, file_id, &parsed).expect("edges");
        assert_eq!(graph.edges().len(), 1);
        assert_eq!(graph.module_count(), 1);
    }
}
